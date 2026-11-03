import type { Repository, RepositorySaveReceipt } from './api';

export type CredentialInputs = { svn_password?: string; git_token?: string };
export type PublicConfiguration = Partial<Repository>;
export type ImportPreferences = { importMode: string; svnRevision: string };
export type SaveStage = 'pending' | 'saved' | 'unknown' | 'failed' | 'conflict';
export interface RepositorySaveAttempt {
  version: 1;
  owner: string;
  mode: 'create' | 'update';
  repoId?: string;
  configuration: PublicConfiguration;
  expectedUpdatedAt?: string;
  configRequestId: string;
  credentialRequestId: string;
  credentialFields: Array<keyof CredentialInputs>;
  configurationStage: SaveStage;
  credentialStage: SaveStage | 'not_required';
  configurationReceipt?: RepositorySaveReceipt;
  protocolFailure?: 'configuration' | 'credentials';
  draftConfiguration?: PublicConfiguration;
  preferences?: ImportPreferences;
  draftPreferences?: ImportPreferences;
  priorAttempts?: RepositorySaveEvidence[];
}

type RepositorySaveEvidence = Pick<RepositorySaveAttempt, 'repoId' | 'configuration' | 'configRequestId' | 'credentialRequestId' | 'expectedUpdatedAt' | 'configurationStage' | 'credentialStage' | 'configurationReceipt' | 'protocolFailure'>;
function saveEvidence(a: RepositorySaveEvidence): RepositorySaveEvidence {
  return {repoId:a.repoId,configuration:publicConfiguration(a.configuration),configRequestId:a.configRequestId,
    credentialRequestId:a.credentialRequestId,expectedUpdatedAt:a.expectedUpdatedAt,configurationStage:a.configurationStage,
    credentialStage:a.credentialStage,configurationReceipt:a.configurationReceipt ? cleanReceipt(a.configurationReceipt) : undefined,protocolFailure:a.protocolFailure};
}
export function repositorySaveConflict(a: RepositorySaveAttempt): boolean {
  return a.configurationStage==='conflict' || a.credentialStage==='conflict' || !!a.protocolFailure;
}

export interface RepositoryEditorIdentity {owner: string; targetId?: string; session: string | null}
export function repositoryEditorKey(value: RepositoryEditorIdentity): string { return JSON.stringify([value.owner,value.targetId,value.session]); }
/** Used at the actual keyed editor/hook boundary; retired scopes cannot send or update replacement views. */
export class RepositoryEditorOwnership {
  private retired=false;
  constructor(private readonly origin: RepositoryEditorIdentity, private readonly current:()=>RepositoryEditorIdentity) {}
  isCurrent(): boolean {return !this.retired && repositoryEditorKey(this.origin)===repositoryEditorKey(this.current());}
  assertCurrent(): void {if(!this.isCurrent())throw new Error('The repository or login context changed. Reopen the correct repository; the earlier recovery record was retained.');}
  retire(): void {this.retired=true;}
  setActive(active: boolean): void {this.retired=!active;}
}

const PUBLIC_FIELDS = [
  'name', 'svn_url', 'svn_branch', 'svn_username', 'git_provider', 'git_api_url',
  'git_repo', 'git_branch', 'sync_mode', 'poll_interval_secs', 'lfs_threshold_mb',
  'auto_merge', 'enabled', 'allowed_paths', 'blocked_patterns',
] as const;

/** Explicit allowlist: credential inputs, tokens, receipts and runtime counters never enter a journal. */
export function publicConfiguration(input: PublicConfiguration): PublicConfiguration {
  const output: Record<string, unknown> = {};
  for (const field of PUBLIC_FIELDS) {
    const value = input[field];
    if (value !== undefined) {
      if(value!==null && !['string','number','boolean'].includes(typeof value))throw new Error('Invalid nonsecret configuration field.');
      output[field] = value;
    }
  }
  for (const field of ['svn_url', 'git_api_url'] as const) {
    const value=output[field];
    if(typeof value!=='string' || !value)continue;
    let url: URL;
    try {url=new URL(value);} catch {
      if(/^[a-z][a-z0-9+.-]*:\/\/[^/]*@/i.test(value)) throw new Error('Use the dedicated credential fields instead of credentials embedded in a URL.');
      continue;
    }
    if(url.username || url.password || url.search || url.hash) throw new Error('Use literal repository URLs without embedded credentials, query credentials or fragments.');
  }
  return output as PublicConfiguration;
}

export interface JournalStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export class RepositorySaveJournal {
  private readonly key: string;
  constructor(private readonly owner: string, private readonly context: string, private readonly storage: JournalStorage) {
    this.key = `repository-save:v1:${encodeURIComponent(owner)}:${encodeURIComponent(context)}`;
  }
  read(): RepositorySaveAttempt | null {
    const raw = this.storage.getItem(this.key);
    if (!raw) return null;
    const attempt = JSON.parse(raw) as RepositorySaveAttempt;
    if (attempt.version !== 1 || attempt.owner !== this.owner
        || !['create', 'update'].includes(attempt.mode)
        || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(attempt.configRequestId)
        || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(attempt.credentialRequestId)
        || (this.context.startsWith('repo:') && attempt.repoId !== this.context.slice(5))
        || !['pending','saved','unknown','failed','conflict'].includes(attempt.configurationStage)
        || !['pending','saved','unknown','failed','conflict','not_required'].includes(attempt.credentialStage)
        || !attempt.configuration || typeof attempt.configuration !== 'object'
        || !Array.isArray(attempt.credentialFields)
        || attempt.credentialFields.some(field => !['svn_password', 'git_token'].includes(field))) {
      throw new Error('The saved recovery record is invalid. Keep its repository ID and review it before starting another save.');
    }
    if (attempt.configurationStage === 'saved') checkReceipt(attempt.configurationReceipt, attempt, false);
    return {...attempt, configuration: publicConfiguration(attempt.configuration)};
  }
  write(attempt: RepositorySaveAttempt, replaceDraft = false): void {
    const current=this.read();
    if (!replaceDraft && current && current.configRequestId===attempt.configRequestId) {
      attempt={...attempt,draftConfiguration:current.draftConfiguration ?? attempt.draftConfiguration,draftPreferences:current.draftPreferences ?? attempt.draftPreferences};
    }
    // Construct the record explicitly; do not serialize a form or a mutation's variables.
    this.storage.setItem(this.key, JSON.stringify({
      version: 1, owner: this.owner, mode: attempt.mode, repoId: attempt.repoId,
      configuration: publicConfiguration(attempt.configuration), draftConfiguration: attempt.draftConfiguration ? publicConfiguration(attempt.draftConfiguration) : undefined, expectedUpdatedAt: attempt.expectedUpdatedAt,
      configRequestId: attempt.configRequestId, credentialRequestId: attempt.credentialRequestId,
      credentialFields: attempt.credentialFields, configurationStage: attempt.configurationStage,
      credentialStage: attempt.credentialStage, protocolFailure: attempt.protocolFailure, configurationReceipt: attempt.configurationReceipt ? cleanReceipt(attempt.configurationReceipt) : undefined,
      preferences: attempt.preferences, draftPreferences:attempt.draftPreferences, priorAttempts:attempt.priorAttempts?.map(saveEvidence),
    }));
  }
  rememberDraft(configuration: PublicConfiguration, preferences?: ImportPreferences): void {
    const saved=this.read();
    if(saved)this.write({...saved,draftConfiguration:publicConfiguration(configuration),draftPreferences:preferences ?? saved.draftPreferences},true);
  }
  clear(): void { this.storage.removeItem(this.key); }
}

function newSaveId(): string {
  const bytes=crypto.getRandomValues(new Uint8Array(16));
  bytes[6]=(bytes[6]&0x0f)|0x40;bytes[8]=(bytes[8]&0x3f)|0x80;
  const h=Array.from(bytes,b=>b.toString(16).padStart(2,'0')).join('');
  return `${h.slice(0,8)}-${h.slice(8,12)}-${h.slice(12,16)}-${h.slice(16,20)}-${h.slice(20)}`;
}

export function repositorySaveComplete(attempt: RepositorySaveAttempt): boolean {
  return attempt.configurationStage === 'saved' && ['saved','not_required'].includes(attempt.credentialStage);
}

export function beginRepositorySave(owner: string, mode: 'create' | 'update', configuration: PublicConfiguration,
  credentials: CredentialInputs, repoId?: string, expectedUpdatedAt?: string): RepositorySaveAttempt {
  const credentialFields = (['svn_password', 'git_token'] as const).filter(field => !!credentials[field]);
  return {
    version: 1, owner, mode, repoId, configuration: publicConfiguration(configuration), expectedUpdatedAt,
    configRequestId: newSaveId(), credentialRequestId: newSaveId(), credentialFields,
    configurationStage: 'pending', credentialStage: credentialFields.length ? 'pending' : 'not_required',
  };
}

export function prepareRepositorySaveAttempt(previous: RepositorySaveAttempt | null,
  configuration: PublicConfiguration, inputs: CredentialInputs,
  options: {owner: string; targetId?: string; expectedUpdatedAt?: string; preferences?: RepositorySaveAttempt['preferences']}): {attempt: RepositorySaveAttempt; replacePrivateInputs: boolean} {
  const clean=publicConfiguration(configuration);
  // Neither absent target equality nor an ordinary Resume is a reviewed recovery decision.
  if(previous && repositorySaveConflict(previous))return {attempt:previous,replacePrivateInputs:false};
  if(!previous || (repositorySaveComplete(previous) && (!samePublicConfiguration(previous.configuration,clean) || !!inputs.svn_password || !!inputs.git_token))
    ) {
    const attempt=beginRepositorySave(options.owner,previous?.repoId || options.targetId ? 'update':'create',clean,inputs,
      previous?.repoId ?? options.targetId,options.expectedUpdatedAt ?? previous?.configurationReceipt?.configuration_updated_at);
    if(previous && !repositorySaveComplete(previous)) {
      attempt.credentialFields=Array.from(new Set([...previous.credentialFields,...attempt.credentialFields]));
      attempt.credentialStage=attempt.credentialFields.length ? 'pending':'not_required';
    }
    attempt.preferences=options.preferences ?? previous?.preferences;
    attempt.priorAttempts=previous ? [...(previous.priorAttempts ?? []),saveEvidence(previous)] : undefined;
    return {attempt,replacePrivateInputs:true};
  }
  if(!previous.configurationReceipt && previous.configurationStage!=='saved') {
    // A failed/unknown creation must retain both its key and its credential intent,
    // including when reload has deliberately discarded the secret input bytes.
    const fields=Array.from(new Set([...previous.credentialFields,...(['svn_password','git_token'] as const).filter(field=>!!inputs[field])]));
    return {attempt:{...previous,configuration:clean,credentialFields:fields,credentialStage:fields.length ? 'pending':'not_required'},replacePrivateInputs:true};
  }
  return {attempt:previous,replacePrivateInputs:false};
}

/** Explicit user decision after fetching/reviewing current settings on the retained ID. No network writes. */
export function beginReviewedRepositorySave(previous: RepositorySaveAttempt, current: Pick<Repository,'id'|'updated_at'>,
  configuration: PublicConfiguration, inputs: CredentialInputs, owner: string, preferences?: ImportPreferences): RepositorySaveAttempt {
  if(!repositorySaveConflict(previous) || previous.owner!==owner || !previous.repoId || previous.repoId!==current.id
    || !Number.isFinite(Date.parse(current.updated_at)))throw new Error('Review current settings on the retained repository before starting a new save.');
  const next=beginRepositorySave(owner,'update',configuration,inputs,current.id,current.updated_at);
  next.credentialFields=Array.from(new Set([...previous.credentialFields,...next.credentialFields]));
  next.credentialStage=next.credentialFields.length ? 'pending':'not_required';
  next.priorAttempts=[...(previous.priorAttempts ?? []),saveEvidence(previous)];
  next.preferences=preferences ?? previous.draftPreferences ?? previous.preferences;
  return next;
}
export function sameImportPreferences(a?: ImportPreferences,b?: ImportPreferences): boolean {
  return a?.importMode===b?.importMode && a?.svnRevision===b?.svnRevision;
}
/** A preference-only continuation is navigation, never another configuration/credential mutation. */
export function canContinueWithPreferences(a: RepositorySaveAttempt | null, configuration: PublicConfiguration, inputs: CredentialInputs): a is RepositorySaveAttempt {
  return !!a && repositorySaveComplete(a) && !!a.repoId && samePublicConfiguration(a.configuration,configuration)
    && !inputs.svn_password && !inputs.git_token;
}

export interface RepositorySaveTransport {
  configuration(attempt: RepositorySaveAttempt, signal: AbortSignal): Promise<Repository>;
  credentials(attempt: RepositorySaveAttempt, inputs: CredentialInputs, signal: AbortSignal): Promise<{ ok: boolean; save_receipt: RepositorySaveReceipt }>;
  lookup(requestId: string, repoId: string | undefined, signal: AbortSignal): Promise<RepositorySaveReceipt>;
}

export class RepositorySaveFailure extends Error {
  constructor(message: string, readonly attempt: RepositorySaveAttempt) { super(message); }
}

function cleanReceipt(receipt: RepositorySaveReceipt): RepositorySaveReceipt {
  return {
    version: receipt.version, request_id: receipt.request_id, kind: receipt.kind,
    repo_id: receipt.repo_id, configuration_updated_at: receipt.configuration_updated_at,
    credential_fields: [...receipt.credential_fields], propagated_repositories: receipt.propagated_repositories,
  };
}

class SaveConfirmationError extends Error {}

function checkReceipt(receipt: RepositorySaveReceipt | undefined, attempt: RepositorySaveAttempt, credential: boolean): RepositorySaveReceipt {
  const requestId = credential ? attempt.credentialRequestId : attempt.configRequestId;
  const kind = credential ? 'credentials' : attempt.mode === 'create' ? 'create' : 'configuration';
  if (!receipt || receipt.version !== 1 || receipt.request_id !== requestId || receipt.kind !== kind
      || !receipt.repo_id || !Number.isFinite(Date.parse(receipt.configuration_updated_at))
      || !Array.isArray(receipt.credential_fields)
      || receipt.credential_fields.some(field => !['svn_password','git_token'].includes(field))
      || !Number.isInteger(receipt.propagated_repositories) || receipt.propagated_repositories < 0
      || (attempt.repoId && receipt.repo_id !== attempt.repoId)) {
    throw new SaveConfirmationError('Save confirmation did not match this request. The recovery record was retained.');
  }
  if (credential && [...receipt.credential_fields].sort().join() !== [...attempt.credentialFields].sort().join()) {
    throw new SaveConfirmationError('Credential confirmation did not match the requested fields.');
  }
  return cleanReceipt(receipt);
}

/** A transport timeout is unknown, never evidence that the server rolled back. */
async function observedRequest<T>(request: (signal: AbortSignal) => Promise<T>): Promise<T> {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout>;
  const timeout = new Promise<never>((_,reject) => {
    timer=setTimeout(() => {controller.abort();reject(new Error('The save response timed out; its server outcome is unknown. Resume this request instead of creating another.'));},30_000);
  });
  try { return await Promise.race([request(controller.signal),timeout]); } finally { clearTimeout(timer!); }
}

/** The journal is updated before/after each effect, independently of page lifetime. */
export async function runRepositorySave(attempt: RepositorySaveAttempt, inputs: CredentialInputs,
  journal: RepositorySaveJournal, transport: RepositorySaveTransport,
  changed: (state: RepositorySaveAttempt) => void = () => {}): Promise<RepositorySaveAttempt> {
  let state = { ...attempt };
  const store = () => { changed({ ...state }); journal.write(state); };
  store(); // Storage failures prevent the first network mutation.
  if(repositorySaveConflict(state) && !state.protocolFailure)throw new RepositorySaveFailure('This save conflicts with current settings. Load and review the current settings on this existing repository before deliberately starting a new save.',state);
  if(state.protocolFailure) throw new RepositorySaveFailure('This server did not provide the required durable save confirmation. No write will be blindly retried. Review the existing repository ID and use a matching API version before a new save.',state);
  { // Confirm the configuration receipt on every resume; journal flags are not server proof.
    let responseId: string | undefined;
    try {
      const repository = await observedRequest(signal => transport.configuration(state, signal));
      responseId=typeof repository.id === 'string' ? repository.id : undefined;
      const receipt = checkReceipt(repository.save_receipt, state, false);
      state = { ...state, repoId: receipt.repo_id, configurationReceipt: receipt, configurationStage: 'saved' };
      store(); // Retain the created ID before any credential request can start.
    } catch (error) {
      state = { ...state, configurationStage: error instanceof SaveConfirmationError || (error as {status?:number}).status===409 ? 'conflict' : 'unknown',
        ...(error instanceof SaveConfirmationError ? {protocolFailure:'configuration' as const,repoId:state.repoId ?? responseId} : {}) };
      // A reused key may have committed an earlier draft; preserve its ID without
      // claiming that newly edited fields were applied or proceeding to credentials.
      if ((error as { status?: number }).status === 409) {
        try {
          const prior = await observedRequest(signal => transport.lookup(state.configRequestId, state.repoId, signal));
          checkReceipt(prior, state, false);
          state = { ...state, repoId: prior.repo_id, configurationStage: 'conflict', configurationReceipt: prior };
        } catch { /* The original error remains visible and the old identity stays retained. */ }
      }
      store();
      throw new RepositorySaveFailure(`Configuration save is not confirmed${state.repoId ? ` (repository ${state.repoId})` : ''}. ${error instanceof Error ? error.message : 'Retry the same save request.'}`, state);
    }
  }
  if (state.credentialStage !== 'not_required') {
    try {
      const supplied = state.credentialFields.every(field => !!inputs[field]);
      let receipt: RepositorySaveReceipt;
      if (supplied) {
        // Even a duplicate must verify the credential payload; it must never
        // silently treat newly entered different bytes as already saved.
        receipt = (await observedRequest(signal => transport.credentials(state, Object.fromEntries(state.credentialFields.map(field => [field, inputs[field]])), signal))).save_receipt;
      } else {
        if (inputs.svn_password || inputs.git_token) {
          throw new Error('Re-enter every originally required credential field, or clear the credential inputs to confirm the earlier save. Partially entered new values have not been saved.');
        }
        try {
          receipt = await observedRequest(signal => transport.lookup(state.credentialRequestId, state.repoId, signal));
        } catch (error) {
          if ((error as { status?: number }).status === 404) {
            throw new Error('Configuration is saved. Re-enter the required credentials to resume; credentials are never stored in browser recovery data.');
          }
          throw error;
        }
      }
      checkReceipt(receipt, state, true);
      state = { ...state, credentialStage: 'saved' };
      store();
    } catch (error) {
      state = { ...state, credentialStage: (error as {status?:number}).status===409 ? 'conflict' : 'unknown',...(error instanceof SaveConfirmationError ? {protocolFailure:'credentials' as const} : {}) }; store();
      throw new RepositorySaveFailure(`Configuration saved on repository ${state.repoId}; credentials are not confirmed. ${error instanceof Error ? error.message : 'Retry credentials on this existing ID.'}`, state);
    }
  }
  return state;
}

export function samePublicConfiguration(left: PublicConfiguration, right: PublicConfiguration): boolean {
  const canonical=(value: PublicConfiguration) => JSON.stringify(Object.fromEntries(Object.entries(publicConfiguration(value)).sort(([a],[b])=>a.localeCompare(b))));
  return canonical(left)===canonical(right);
}

/** Only a complete potentially submitted payload is immutable. Empty/partial inputs never authorize a credential POST. */
export function selectRepositoryCredentialInputs(cached: CredentialInputs | null, inputs: CredentialInputs,
  required: Array<keyof CredentialInputs>, replace: boolean): {cached: CredentialInputs | null; submitted: CredentialInputs} {
  const complete=(value: CredentialInputs | null): value is CredentialInputs => !!value && required.length>0 && required.every(field=>!!value[field]);
  if(!replace && complete(cached))return {cached,submitted:cached};
  const selected={...inputs};
  return {cached:complete(selected) ? selected:null,submitted:selected};
}

export function submittedCredentialSnapshot(attempt: RepositorySaveAttempt, inputs: CredentialInputs): CredentialInputs {
  if(!attempt.credentialFields.every(field=>!!inputs[field]))return {};
  return Object.fromEntries(attempt.credentialFields.map(field=>[field,inputs[field]]));
}

export function retainUnsavedCredentials(current: {svn_password: string; git_token: string}, confirmed: CredentialInputs) {
  return {
    svn_password:current.svn_password===confirmed.svn_password ? '' : current.svn_password,
    git_token:current.git_token===confirmed.git_token ? '' : current.git_token,
  };
}
