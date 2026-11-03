import { useEffect, useMemo, useState } from 'react';
import { api } from './api';
import {
  publicConfiguration, prepareRepositorySaveAttempt, RepositorySaveFailure, RepositorySaveJournal,
  repositorySaveComplete, runRepositorySave, submittedCredentialSnapshot,
  type CredentialInputs, type PublicConfiguration, type RepositorySaveAttempt, type RepositorySaveTransport,
} from './repositorySave';
import { getStoredUser } from './utils/auth';

const flights = new Map<string, Promise<SaveResult>>();
interface SaveResult {
  attempt: RepositorySaveAttempt;
  configuration: PublicConfiguration;
  credentials: CredentialInputs;
  viewActive: boolean;
}

export function useRepositorySave(targetId: string | undefined, visible: boolean) {
  const owner = getStoredUser()?.id ?? 'legacy';
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
    return { key:`${owner}:${name}`, journal, initial, error, active:visible, session:localStorage.getItem('session_token'), privateInputs:null as CredentialInputs | null };
  },[owner,targetId,visible]);
  const [attempt,setAttempt]=useState<RepositorySaveAttempt | null>(context.initial);
  const [error,setError]=useState<string | null>(context.error);
  const [pending,setPending]=useState(false);
  useEffect(() => {
    context.active=visible;setAttempt(context.initial);setError(context.error);
    const flight=flights.get(context.key);setPending(!!flight);
    if (flight) void flight.then(result=>{if (context.active)setAttempt(result.attempt);},cause=>{
      if(context.active)setError(cause instanceof Error ? cause.message : 'Save is not confirmed.');
    }).finally(()=>{if(context.active)setPending(false);});
    return () => {context.active=false;context.privateInputs=null;};
  },[context,visible]);

  const transport = (session: string | null): RepositorySaveTransport => {
    const check = () => {
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
    if (context.error) throw new Error(context.error);
    if (context.session !== localStorage.getItem('session_token') || (getStoredUser()?.id ?? 'legacy') !== owner) throw new Error('The login session changed. Reopen this form to resume as the current user.');
    const existing=flights.get(context.key);
    if (existing) return existing.then(result=>({...result,viewActive:context.active}));
    const session=localStorage.getItem('session_token');
    const operation=(async () => {
      const prepared=prepareRepositorySaveAttempt(context.journal.read(),configuration,inputs,{owner,targetId,expectedUpdatedAt,preferences});
      const state=prepared.attempt;
      if(prepared.replacePrivateInputs)context.privateInputs={...inputs};
      const submitted=context.privateInputs ?? {...inputs};
      setPending(true);setError(null);
      try {
        const final=await runRepositorySave(state,submitted,context.journal,transport(session),next=>{
          if (context.active) setAttempt(next);
        });
        return {attempt:final,configuration:state.configuration,credentials:submittedCredentialSnapshot(state,submitted),viewActive:context.active && localStorage.getItem('session_token')===session};
      } catch (cause) {
        if (context.active) {
          if (cause instanceof RepositorySaveFailure) setAttempt(cause.attempt);
          setError(cause instanceof Error ? cause.message : 'Save could not be confirmed. The recovery record was retained.');
        }
        throw cause;
      } finally {
        if (context.active) setPending(false);
      }
    })().finally(() => { flights.delete(context.key); });
    flights.set(context.key,operation);
    return operation;
  }

  function rememberDraft(configuration: PublicConfiguration) {
    try {
      context.journal.rememberDraft(publicConfiguration(configuration));
    } catch (cause) {setError(cause instanceof Error ? cause.message : 'Draft recovery data could not be saved.');}
  }
  function finish(requestId: string) {
    const saved=context.journal.read();
    if (saved?.configRequestId===requestId && repositorySaveComplete(saved)) context.journal.clear();
    context.privateInputs=null;
  }
  return {attempt,initial:context.initial,error,pending,save,rememberDraft,finish};
}
