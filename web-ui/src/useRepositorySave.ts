import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { api, type Repository } from './api';
import {
  publicConfiguration, prepareRepositorySaveAttempt, RepositorySaveFailure, RepositorySaveJournal,
  repositorySaveComplete, runRepositorySave, submittedCredentialSnapshot, beginReviewedRepositorySave, RepositoryEditorOwnership, selectRepositoryCredentialInputs,
  type CredentialInputs, type PublicConfiguration, type RepositorySaveAttempt, type RepositorySaveTransport,
} from './repositorySave';
import { getStoredUser } from './utils/auth';

const flights = new Map<string, Promise<SaveResult>>();
interface SaveResult {
  attempt: RepositorySaveAttempt;
  configuration: PublicConfiguration;
  credentials: CredentialInputs;
  viewActive: boolean;
  preferences?: RepositorySaveAttempt['preferences'];
}

export function useRepositorySave(targetId: string | undefined, visible: boolean) {
  const owner = getStoredUser()?.id ?? 'legacy';
  const session=localStorage.getItem('session_token');
  const liveScope=useRef({owner,targetId,session});liveScope.current={owner,targetId,session};
  const context = useMemo(() => {
    let name=targetId ? `repo:${targetId}` : 'create';
    let error: string | null=null;
    try {
      const creation=new RepositorySaveJournal(owner,'create',localStorage).read();
      if (targetId && creation?.repoId===targetId && !repositorySaveComplete(creation)) name='create';
    } catch (cause) {error=cause instanceof Error ? cause.message : 'Recovery data could not be read.';}
    const journal=new RepositorySaveJournal(owner,name,localStorage);
    let initial: RepositorySaveAttempt | null=null;
    try {initial=journal.read();} catch (cause) {error=cause instanceof Error ? cause.message : 'Recovery data could not be read.';}
    const ownership=new RepositoryEditorOwnership({owner,targetId,session},()=>({...liveScope.current,owner:getStoredUser()?.id ?? 'legacy',session:localStorage.getItem('session_token')}));
    return { key:`${owner}:${name}`, journal, initial, error, active:visible, ownership, session, privateInputs:null as CredentialInputs | null };
  },[owner,targetId,visible,session]);
  const [attempt,setAttempt]=useState<RepositorySaveAttempt | null>(context.initial);
  const [error,setError]=useState<string | null>(context.error);
  const [pending,setPending]=useState(false);
  const [reviewedCurrent,setReviewedCurrent]=useState<Repository | null>(null);
  useLayoutEffect(() => {
    context.active=visible;context.ownership.setActive(visible);setAttempt(context.initial);setError(context.error);setReviewedCurrent(null);
    const flight=flights.get(context.key);setPending(!!flight);
    if (flight) void flight.then(result=>{if (context.active && context.ownership.isCurrent())setAttempt(result.attempt);},cause=>{
      if(context.active && context.ownership.isCurrent())setError(cause instanceof Error ? cause.message : 'Save is not confirmed.');
    }).finally(()=>{if(context.active && context.ownership.isCurrent())setPending(false);});
    return () => {context.active=false;context.ownership.retire();context.privateInputs=null;};
  },[context,visible]);

  const transport = (session: string | null): RepositorySaveTransport => {
    const check = () => {
      context.ownership.assertCurrent();
      if (localStorage.getItem('session_token')!==session || (getStoredUser()?.id ?? 'legacy') !== owner) throw new Error('The login session changed. The save is retained for its original user.');
    };
    return {
      configuration: (state,signal) => {
        check();
        return state.mode==='create'
          ? api.createRepo(state.configuration,state.configRequestId,signal)
          : api.updateRepo(state.repoId!,state.configuration,{requestId:state.configRequestId,expectedUpdatedAt:state.expectedUpdatedAt,signal});
      },
      credentials: (state,inputs,signal) => {
        check();return api.saveRepoCredentials(state.repoId!,inputs,state.credentialRequestId,state.configurationReceipt?.configuration_updated_at,signal);
      },
      lookup: (requestId,repoId,signal) => {check();return api.getRepoSaveRequest(requestId,repoId,signal);},
    };
  };

  async function save(configuration: PublicConfiguration, inputs: CredentialInputs, expectedUpdatedAt?: string,
    preferences?: RepositorySaveAttempt['preferences']): Promise<SaveResult> {
    context.ownership.assertCurrent();
    if (context.error) throw new Error(context.error);
    if (context.session !== localStorage.getItem('session_token') || (getStoredUser()?.id ?? 'legacy') !== owner) throw new Error('The login session changed. Reopen this form to resume as the current user.');
    const existing=flights.get(context.key);
    if (existing) return existing.then(result=>({...result,viewActive:context.active && context.ownership.isCurrent()}));
    const session=localStorage.getItem('session_token');
    const operation=(async () => {
      const prepared=prepareRepositorySaveAttempt(context.journal.read(),configuration,inputs,{owner,targetId,expectedUpdatedAt,preferences});
      const state=prepared.attempt;
      const selected=selectRepositoryCredentialInputs(context.privateInputs,inputs,state.credentialFields,prepared.replacePrivateInputs);
      context.privateInputs=selected.cached;
      const submitted=selected.submitted;
      setPending(true);setError(null);
      try {
        const final=await runRepositorySave(state,submitted,context.journal,transport(session),next=>{
          if (context.active && context.ownership.isCurrent()) setAttempt(next);
        });
        return {attempt:final,configuration:state.configuration,credentials:submittedCredentialSnapshot(state,submitted),preferences:state.preferences,viewActive:context.active && context.ownership.isCurrent() && localStorage.getItem('session_token')===session};
      } catch (cause) {
        if (context.active && context.ownership.isCurrent()) {
          if (cause instanceof RepositorySaveFailure) setAttempt(cause.attempt);
          setError(cause instanceof Error ? cause.message : 'Save could not be confirmed. The recovery record was retained.');
        }
        throw cause;
      } finally {
        if (context.active && context.ownership.isCurrent()) setPending(false);
      }
    })().finally(() => { flights.delete(context.key); });
    flights.set(context.key,operation);
    return operation;
  }

  function rememberDraft(configuration: PublicConfiguration, preferences?: RepositorySaveAttempt['preferences']) {
    try {
      context.ownership.assertCurrent();context.journal.rememberDraft(publicConfiguration(configuration),preferences);
    } catch (cause) {setError(cause instanceof Error ? cause.message : 'Draft recovery data could not be saved.');}
  }
  async function loadCurrentForReview(): Promise<Repository> {
    context.ownership.assertCurrent();
    const previous=context.journal.read();
    if(!previous?.repoId || flights.has(context.key))throw new Error('A known existing repository and an idle save are required.');
    const current=await api.getRepo(previous.repoId);
    context.ownership.assertCurrent();
    if(current.id!==previous.repoId)throw new Error('Current settings belong to a different repository.');
    setReviewedCurrent(current);return current;
  }
  function useReviewedSettings(configuration: PublicConfiguration, inputs: CredentialInputs, preferences?: RepositorySaveAttempt['preferences']) {
    context.ownership.assertCurrent();
    const previous=context.journal.read();
    if(!previous || !reviewedCurrent || flights.has(context.key))throw new Error('Load and review current settings before starting a new save.');
    const next=beginReviewedRepositorySave(previous,reviewedCurrent,configuration,inputs,owner,preferences);
    context.journal.write(next,true);context.privateInputs=null;setAttempt(next);setError(null);setReviewedCurrent(null);
  }
  function finish(requestId: string) {
    context.ownership.assertCurrent();
    const saved=context.journal.read();
    if (saved?.configRequestId===requestId && repositorySaveComplete(saved)) context.journal.clear();
    context.privateInputs=null;
  }
  return {attempt,initial:context.initial,error,pending,save,rememberDraft,finish,loadCurrentForReview,useReviewedSettings,reviewedCurrent,viewActive:()=>context.active && context.ownership.isCurrent()};
}
