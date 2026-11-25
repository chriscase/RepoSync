import { createElement } from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { api, type User } from '../api';
import AdministratorRoute from './AdministratorRoute';

const identity = (role: string): User => ({
  id: 'fixture', username: 'fixture', display_name: 'Fixture', role,
  email: '', enabled: true, created_at: '',
});
const storage = new Map<string, string>();
let tree: ReactTestRenderer;

beforeEach(() => {
  storage.clear();
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null });
  vi.stubGlobal('window', new EventTarget());
});
afterEach(() => {
  act(() => tree?.unmount());
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function mount() {
  await act(async () => {
    tree = create(createElement(AdministratorRoute, { children: createElement('form') }));
  });
}
function hasForm() {
  return tree.root.findAllByType('form').length === 1;
}
function notify() {
  window.dispatchEvent(new Event('storage'));
}

it('refuses non-administrators and missing sessions before mounting the page', async () => {
  const me = vi.spyOn(api, 'getMe');
  await mount();
  expect(hasForm()).toBe(false);
  storage.set('session_token', 'session');
  storage.set('user', JSON.stringify(identity('user')));
  await act(async () => notify());
  expect(hasForm()).toBe(false);
  expect(me).not.toHaveBeenCalled();
});

it.each(['user', 'error'])('denies an unverified stored admin when the server returns %s', async result => {
  storage.set('session_token', 'session');
  storage.set('user', JSON.stringify(identity('admin')));
  const me = vi.spyOn(api, 'getMe');
  if (result === 'error') me.mockRejectedValue(new Error('Unavailable'));
  else me.mockResolvedValue(identity(result));
  await mount();
  expect(tree.root.findByProps({ role: 'alert' })).toBeDefined();
  expect(hasForm()).toBe(false);
});

it('admits a confirmed legacy administrator and immediately removes forms after demotion', async () => {
  storage.set('session_token', 'legacy');
  const me = vi.spyOn(api, 'getMe').mockResolvedValue(identity('admin'));
  await mount();
  expect(hasForm()).toBe(true);
  storage.set('user', JSON.stringify(identity('user')));
  await act(async () => notify());
  expect(hasForm()).toBe(false);
  expect(me).toHaveBeenCalledTimes(1);
});

it('verifies each session separately and ignores a response for the previous session', async () => {
  let completePrevious!: (user: User) => void;
  storage.set('session_token', 'first');
  const me = vi.spyOn(api, 'getMe')
    .mockReturnValueOnce(new Promise<User>(resolve => { completePrevious = resolve; }))
    .mockResolvedValueOnce(identity('user'));
  await mount();
  expect(tree.root.findByProps({ role: 'status' })).toBeDefined();
  storage.set('session_token', 'second');
  await act(async () => notify());
  await act(async () => completePrevious(identity('admin')));
  expect(hasForm()).toBe(false);
  expect(me).toHaveBeenCalledTimes(2);
});
