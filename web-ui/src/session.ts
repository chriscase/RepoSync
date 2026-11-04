/** Client identity boundaries. Cancellation never proves a server mutation rolled back. */
export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message: string) { super(message); this.status = status; }
}
export class RetiredSessionError extends Error {
  constructor() { super('The login session changed. Earlier requests cannot update this session; a submitted mutation may still need reconciliation.'); }
}
export interface SessionSnapshot {
  readonly epoch: number;
  readonly token: string | null;
  readonly user: string | null;
  readonly boundary: string | null;
}
type SessionStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
const BOUNDARY_KEY = 'reposync.session-boundary';
// An opaque cross-tab incarnation marker, not a UUID or server authorization token.
// getRandomValues also works on the project's documented non-localhost HTTP deployment.
function boundaryNonce(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
}

function principal(raw: string | null): string {
  try { const user = JSON.parse(raw ?? 'null'); return JSON.stringify([user?.id, user?.role]); }
  catch { return 'unknown'; }
}

export class ClientSession {
  private readonly storage: SessionStorage;
  private value: SessionSnapshot;
  private listeners = new Set<() => void>();
  private requests = new Set<AbortController>();
  constructor(storage: SessionStorage) {
    this.storage = storage;
    this.value = { epoch: 0, ...this.read() };
  }
  private read() {
    const token = this.storage.getItem('session_token');
    return { token, user: token ? this.storage.getItem('user') : null, boundary: this.storage.getItem(BOUNDARY_KEY) };
  }
  /** Also fences a response if another tab changed storage before its event was delivered. */
  readonly snapshot = (): SessionSnapshot => {
    const next = this.read();
    if (next.token !== this.value.token || next.user !== this.value.user || next.boundary !== this.value.boundary) {
      const changed = next.token !== this.value.token || next.boundary !== this.value.boundary || principal(next.user) !== principal(this.value.user);
      this.value = { ...next, epoch: this.value.epoch + (changed ? 1 : 0) };
      if (changed) { for (const request of this.requests) request.abort(); this.requests.clear(); }
      for (const notify of this.listeners) notify();
    }
    return this.value;
  };
  readonly subscribe = (notify: () => void) => { this.listeners.add(notify); return () => { this.listeners.delete(notify); }; };
  isCurrent(origin: SessionSnapshot): boolean { return this.snapshot().epoch === origin.epoch; }
  assertCurrent(origin: SessionSnapshot): void { if (!this.isCurrent(origin)) throw new RetiredSessionError(); }
  track(origin: SessionSnapshot, controller: AbortController): () => void {
    this.assertCurrent(origin); this.requests.add(controller);
    return () => { this.requests.delete(controller); };
  }
  signIn(result: { token: string; user?: unknown }, origin: SessionSnapshot): void {
    this.assertCurrent(origin);
    if (!result.token) throw new Error('Login did not return a session.');
    // Never pair a replacement token with the preceding user's role if storage fails.
    try {
      this.storage.removeItem('session_token'); this.storage.removeItem('user');
      if (result.user) this.storage.setItem('user', JSON.stringify(result.user));
      else this.storage.removeItem('user'); // Legacy login must never inherit the preceding administrator.
      this.storage.setItem(BOUNDARY_KEY, boundaryNonce());
      this.storage.setItem('session_token', result.token);
    } finally { this.snapshot(); }
  }
  end(origin = this.snapshot()): string | null {
    this.assertCurrent(origin);
    try {
      this.storage.removeItem('session_token'); this.storage.removeItem('user');
      this.storage.setItem(BOUNDARY_KEY, boundaryNonce());
    } finally { this.snapshot(); }
    return origin.token;
  }
  updateUser(user: unknown, origin: SessionSnapshot): void {
    this.assertCurrent(origin); this.storage.setItem('user', JSON.stringify(user)); this.snapshot();
  }
}

let current: ClientSession | undefined;
let storageObject: Storage | undefined;
/** One controller per browser storage surface. Laziness also permits server rendering. */
export function clientSession(): ClientSession {
  if (!current || storageObject !== localStorage) {
    storageObject = localStorage; current = new ClientSession(localStorage);
  }
  return current;
}
export function watchSessionStorage(target: Pick<Window, 'addEventListener' | 'removeEventListener'> = window): () => void {
  const changed = (event: StorageEvent) => {
    if (event.key === null || ['session_token', 'user', BOUNDARY_KEY].includes(event.key)) clientSession().snapshot();
  };
  target.addEventListener('storage', changed);
  return () => { target.removeEventListener('storage', changed); };
}

function aborted<T>(operation: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const stop = () => reject(new DOMException('Request retired or cancelled', 'AbortError'));
    signal.addEventListener('abort', stop, { once: true });
    if (signal.aborted) stop();
    operation.then(resolve, reject).finally(() => signal.removeEventListener('abort', stop));
  });
}

/** All application fetches use this boundary, including specialized 202/409 readers. */
export async function sessionFetch(url: string, options: RequestInit = {}, authenticated = true): Promise<Response> {
  const session = clientSession(); const origin = session.snapshot(); const controller = new AbortController();
  const release = session.track(origin, controller);
  const stop = () => controller.abort(); options.signal?.addEventListener('abort', stop, { once: true });
  if (options.signal?.aborted) controller.abort();
  const finish = () => { release(); options.signal?.removeEventListener('abort', stop); };
  const check = () => { session.assertCurrent(origin); if (controller.signal.aborted || options.signal?.aborted) throw new DOMException('Request cancelled', 'AbortError'); };
  const headers = new Headers(options.headers);
  if (authenticated) {
    headers.delete('Authorization');
    if (origin.token) headers.set('Authorization', `Bearer ${origin.token}`);
  }
  try {
    check();
    const incoming = await aborted(fetch(url, { ...options, headers, signal: controller.signal }), controller.signal);
    check();
    // All current consumers are JSON/text readers. Keep ownership through the body,
    // including early-return 404/202 consumers, then retain origin checks on decoding.
    const body = await aborted(incoming.text(), controller.signal); check();
    const response = new Response([204, 205, 304].includes(incoming.status) ? null : body, { status: incoming.status, statusText: incoming.statusText, headers: incoming.headers });
    finish();
    if (authenticated && response.status === 401) {
      // Older role-checked endpoints return this exact JSON denial as 401.
      // Keep the denied operation failed without expiring a valid session; this grants no access.
      const text = body;
      let message: unknown;
      try { message = (JSON.parse(text) as { error?: unknown }).error; } catch { /* An ordinary 401 still expires this origin. */ }
      if (message === 'admin access required') throw new ApiError(403, 'API error 403: admin access required');
      session.end(origin);
      if (typeof window !== 'undefined' && window.location.pathname !== '/login') window.location.href = '/login';
      throw new ApiError(401, 'Session expired — please log in again');
    }
    // Body reads can finish after headers, cancellation or account replacement.
    for (const method of ['json', 'text'] as const) {
      const read = response[method].bind(response);
      Object.defineProperty(response, method, { value: async () => {
        try { check(); const value = await aborted(read(), controller.signal); check(); return value; }
        finally { finish(); }
      } });
    }
    return response;
  } catch (error) { finish(); throw error; }
}

/** Display receipts are partitioned; legacy unscoped data is retained but never shown to another identity. */
export function accountStorageKey(base: string): string {
  if (typeof localStorage === 'undefined') return `${base}anonymous.`;
  const value = clientSession().snapshot();
  if (!value.token) return `${base}anonymous.`;
  const identity = `${principal(value.user)}:${value.user ? '' : value.boundary ?? 'legacy'}`;
  return `${base}account:${encodeURIComponent(identity)}.`;
}
