import { afterEach, describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import Config from './Config';
import SetupWizard from './SetupWizard';
import { clientSession } from '../session';

const clients: QueryClient[] = [];
afterEach(() => { for (const client of clients.splice(0)) client.clear(); vi.unstubAllGlobals(); });
function render(path: string, role: string | null, staleIdentity: false | 'error' | 'missing-token' = false) {
  const entries = new Map([['session_token', 'synthetic-only']]);
  if (staleIdentity === 'missing-token') entries.delete('session_token');
  if (role !== null) entries.set('user', JSON.stringify({id:'synthetic-user',username:'fixture',role}));
  vi.stubGlobal('localStorage', {getItem:(key:string)=>entries.get(key) ?? null,setItem:(key:string,value:string)=>entries.set(key,value),removeItem:(key:string)=>entries.delete(key)});
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
  const client = new QueryClient({defaultOptions:{queries:{retry:false,staleTime:Infinity}}}); clients.push(client);
  client.setQueryData(['identityMappings'], []);
  if (staleIdentity) {
    const queryKey = ['admin-page-identity', clientSession().snapshot().epoch];
    client.setQueryData(queryKey, {id:'legacy',username:'admin',role:'admin'});
    if (staleIdentity === 'error') client.getQueryCache().find({queryKey})!.setState({status:'error',error:new Error('identity refresh failed'),fetchStatus:'idle'});
  }

  const html = renderToStaticMarkup(createElement(QueryClientProvider,{client},createElement(MemoryRouter,{initialEntries:[path]},createElement(Routes,null,
    createElement(Route,{path:'/config',element:createElement(Config)}),createElement(Route,{path:'/setup',element:createElement(SetupWizard)})))));
  return {html,fetch,client};
}
for (const path of ['/setup','/config']) describe(`#268 direct ${path} entry`, () => {
  for (const role of ['viewer','user','unknown',null]) it(`refuses ${role} before mounting privileged content`, () => {
    const {html,fetch,client} = render(path, role);
    expect(html).toContain(role === null ? 'Verifying administrator access' : 'Access Denied');
    expect(html).not.toMatch(/<form|<input|<button/);
    expect(fetch).not.toHaveBeenCalled();
    expect(client.getQueryCache().getAll().every(query=>query.getObserversCount()===0)).toBe(true);
  });
});

for (const path of ['/setup','/config']) for (const failure of ['error','missing-token'] as const) it(`#268 ${path} refuses retained admin identity after ${failure}`, () => {
  const {html,fetch} = render(path,null,failure);
  expect(html).toContain('Access Denied');expect(html).not.toMatch(/<form|<input|<button/);expect(fetch).not.toHaveBeenCalled();
});
