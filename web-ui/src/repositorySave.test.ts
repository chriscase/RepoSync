import { describe, expect, it } from 'vitest';
import { ApiError, type Repository, type RepositorySaveReceipt } from './api';
import {
  beginReviewedRepositorySave, RepositoryEditorOwnership, repositoryEditorKey, sameImportPreferences, canContinueWithPreferences,
  beginRepositorySave, prepareRepositorySaveAttempt, publicConfiguration, submittedCredentialSnapshot, retainUnsavedCredentials, RepositorySaveJournal, repositorySaveComplete,
  runRepositorySave, samePublicConfiguration,
  type PublicConfiguration, type JournalStorage, type RepositorySaveAttempt, type RepositorySaveTransport,
} from './repositorySave';

class Storage implements JournalStorage {
  values=new Map<string,string>(); failAt=0; writes=0;
  getItem(key:string){return this.values.get(key) ?? null;}
  setItem(key:string,value:string){if(++this.writes===this.failAt)throw new Error('Storage is full');this.values.set(key,value);}
  removeItem(key:string){this.values.delete(key);}
}
const configuration={name:'Example',svn_url:'svn://localhost/project',svn_branch:'trunk',git_repo:'test/repo'};
const secrets={svn_password:'synthetic-password-秘密',git_token:'synthetic-token'};
const time='2026-11-03T18:00:00Z';
function receipt(attempt:RepositorySaveAttempt,credential=false):RepositorySaveReceipt {
  return {version:1,request_id:credential ? attempt.credentialRequestId : attempt.configRequestId,
    kind:credential ? 'credentials' : attempt.mode==='create' ? 'create':'configuration',repo_id:attempt.repoId ?? 'repo-created',
    configuration_updated_at:time,credential_fields:credential ? attempt.credentialFields : [],propagated_repositories:0};
}
function fixture(){
  const storage=new Storage();const journal=new RepositorySaveJournal('actor','create',storage);
  const attempt=beginRepositorySave('actor','create',configuration,secrets);
  const commits=new Map<string,{receipt:RepositorySaveReceipt,payload:string}>();
  const state={configCalls:0,credentialCalls:0,configEffects:0,credentialEffects:0,configFail:false,credentialFail:false,loseConfig:false,loseCredentials:false,secretValue:'',gate:null as Promise<void>|null};
  const transport:RepositorySaveTransport={
    configuration:async a=>{
      state.configCalls++;if(state.gate)await state.gate;if(state.configFail)throw new ApiError(500,'configuration failed');
      const payload=JSON.stringify(a.configuration);let known=commits.get(a.configRequestId);
      if(known && known.payload!==payload)throw new ApiError(409,'request changed');
      if(!known){known={receipt:receipt(a),payload};commits.set(a.configRequestId,known);state.configEffects++;}
      if(state.loseConfig){state.loseConfig=false;throw new TypeError('response lost');}
      return {id:known.receipt.repo_id,updated_at:time,save_receipt:known.receipt} as Repository;
    },
    credentials:async(a,input)=>{
      state.credentialCalls++;if(state.credentialFail)throw new ApiError(500,'credential storage failed');
      const payload=JSON.stringify(input);let known=commits.get(a.credentialRequestId);
      if(known && known.payload!==payload)throw new ApiError(409,'different credential payload');
      if(!known){known={receipt:receipt(a,true),payload};commits.set(a.credentialRequestId,known);state.credentialEffects++;state.secretValue=payload;}
      if(state.loseCredentials){state.loseCredentials=false;throw new TypeError('credential response lost');}
      return {ok:true,save_receipt:known.receipt};
    },
    lookup:async id=>{const known=commits.get(id);if(!known)throw new ApiError(404,'request not found');return known.receipt;},
  };
  return {storage,journal,attempt,commits,state,transport};
}

describe('#115 directly used repository save protocol',()=>{
  it('does not start a successful credential request when configuration fails',async()=>{
    const f=fixture();f.state.configFail=true;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('Configuration save is not confirmed');
    expect(f.state.credentialCalls).toBe(0);expect(f.state.credentialEffects).toBe(0);
    expect(f.journal.read()?.configuration).toEqual(configuration);
  });
  it('cannot finish credentials before a delayed configuration response',async()=>{
    const f=fixture();let release!:()=>void;f.state.gate=new Promise<void>(resolve=>{release=resolve;});
    const running=runRepositorySave(f.attempt,secrets,f.journal,f.transport);
    await Promise.resolve();expect(f.state.credentialCalls).toBe(0);expect(f.journal.read()?.configurationStage).toBe('pending');
    release();const final=await running;expect(repositorySaveComplete(final)).toBe(true);expect(f.state.configEffects).toBe(1);expect(f.state.credentialEffects).toBe(1);
  });
  it('retains the created ID before credential failure and retries only the existing registration',async()=>{
    const f=fixture();f.state.credentialFail=true;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('Configuration saved on repository repo-created');
    const pending=f.journal.read()!;expect(pending.repoId).toBe('repo-created');expect(pending.configurationStage).toBe('saved');expect(pending.credentialStage).toBe('unknown');
    f.state.credentialFail=false;const final=await runRepositorySave(pending,secrets,f.journal,f.transport);
    expect(repositorySaveComplete(final)).toBe(true);expect(final.repoId).toBe(pending.repoId);
    expect(f.state.configEffects).toBe(1);expect(f.state.credentialEffects).toBe(1);
    expect(final.configRequestId).toBe(f.attempt.configRequestId);expect(final.credentialRequestId).toBe(f.attempt.credentialRequestId);
  });
  it('uses the same create identity after a lost response and does not register twice',async()=>{
    const f=fixture();f.state.loseConfig=true;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('response lost');
    expect(f.journal.read()?.repoId).toBeUndefined();
    const recovered=await runRepositorySave(f.journal.read()!,secrets,f.journal,f.transport);
    expect(recovered.repoId).toBe('repo-created');expect(f.state.configEffects).toBe(1);expect(f.state.credentialEffects).toBe(1);
  });
  it('confirms lost credential responses after reload without persisting or replaying secret bytes',async()=>{
    const f=fixture();f.state.loseCredentials=true;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('credentials are not confirmed');
    const serialized=[...f.storage.values.values()].join();expect(serialized).not.toContain(secrets.svn_password);expect(serialized).not.toContain(secrets.git_token);
    f.state.secretValue='a later administrator value';
    const final=await runRepositorySave(f.journal.read()!,{},f.journal,f.transport);
    expect(repositorySaveComplete(final)).toBe(true);expect(f.state.credentialEffects).toBe(1);expect(f.state.secretValue).toBe('a later administrator value');
  });
  it('rejects a changed credential payload under an already committed request identity',async()=>{
    const f=fixture();const saved=await runRepositorySave(f.attempt,secrets,f.journal,f.transport);
    await expect(runRepositorySave(saved,{...secrets,git_token:'different'},f.journal,f.transport)).rejects.toThrow('different credential payload');
    expect(f.state.credentialEffects).toBe(1);
  });
  it('requires credential re-entry when the previous credential stage never committed',async()=>{
    const f=fixture();f.state.credentialFail=true;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow();
    f.state.credentialFail=false;
    await expect(runRepositorySave(f.journal.read()!,{},f.journal,f.transport)).rejects.toThrow('Re-enter the required credentials');
    expect(f.state.credentialEffects).toBe(0);expect(f.journal.read()?.repoId).toBe('repo-created');
  });
  it('keeps the journal after navigation and does not require a live view to record completion',async()=>{
    const f=fixture();let release!:()=>void;f.state.gate=new Promise<void>(resolve=>{release=resolve;});
    let mounted=true;let visibleUpdates=0;
    const running=runRepositorySave(f.attempt,secrets,f.journal,f.transport,()=>{if(mounted)visibleUpdates++;});
    const before=visibleUpdates;mounted=false;release();await running;
    expect(visibleUpdates).toBe(before);expect(repositorySaveComplete(f.journal.read()!)).toBe(true);
    expect(f.journal.read()?.repoId).toBe('repo-created');expect(f.state.configEffects).toBe(1);
  });
  it('preserves newer nonsecret edits while an earlier snapshot is saving',async()=>{
    const f=fixture();let release!:()=>void;f.state.gate=new Promise<void>(resolve=>{release=resolve;});
    const running=runRepositorySave(f.attempt,secrets,f.journal,f.transport);
    const newer={...configuration,name:'Newer unsaved name'};
    const inFlight=f.journal.read()!;inFlight.draftConfiguration=newer;f.journal.write(inFlight);
    release();const final=await running;
    expect(f.journal.read()?.draftConfiguration).toEqual(newer);
    expect(samePublicConfiguration(newer,final.configuration)).toBe(false);
    expect(final.configuration.name).toBe('Example');
  });
  it('blocks all network mutation if the initial journal cannot be stored',async()=>{
    const f=fixture();f.storage.failAt=1;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('Storage is full');
    expect(f.state.configCalls).toBe(0);expect(f.state.credentialCalls).toBe(0);
  });
  it('retains a known created ID in the live view if persistence fails after creation',async()=>{
    const f=fixture();f.storage.failAt=2;let known:RepositorySaveAttempt|undefined;
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport,state=>{known=state;})).rejects.toThrow('Storage is full');
    expect(known?.repoId).toBe('repo-created');expect(f.state.credentialCalls).toBe(0);
    f.storage.failAt=0;await runRepositorySave(f.journal.read()!,secrets,f.journal,f.transport);expect(f.state.configEffects).toBe(1);
  });
  it('confirms server receipts again instead of trusting persisted stage flags',async()=>{
    const f=fixture();const saved=await runRepositorySave(f.attempt,secrets,f.journal,f.transport);
    const before=f.state.configCalls;await runRepositorySave(saved,{},f.journal,f.transport);
    expect(f.state.configCalls).toBe(before+1);expect(f.state.configEffects).toBe(1);expect(f.state.credentialEffects).toBe(1);
  });
  it('does not turn blank optional credential fields into revocation writes',async()=>{
    const f=fixture();const noCredentials=beginRepositorySave('actor','create',configuration,{svn_password:'',git_token:''});
    const result=await runRepositorySave(noCredentials,{},f.journal,f.transport);
    expect(result.credentialStage).toBe('not_required');expect(f.state.credentialCalls).toBe(0);
  });
  it('refuses wrong confirmation identities and never proceeds to credentials',async()=>{
    const f=fixture();f.transport.configuration=async()=>({id:'wrong',save_receipt:{...receipt(f.attempt),request_id:'wrong'}} as Repository);
    await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('did not match');
    expect(f.state.credentialCalls).toBe(0);
  });
  it('never copies unknown secret-bearing response properties into recovery data',async()=>{
    const f=fixture();const original=f.transport.configuration;
    f.transport.configuration=async(a,signal)=>{const repo=await original(a,signal);return {...repo,save_receipt:{...repo.save_receipt!,git_token:'unexpected-secret'}};};
    await runRepositorySave(f.attempt,secrets,f.journal,f.transport);
    expect([...f.storage.values.values()].join()).not.toContain('unexpected-secret');
  });
  it('rejects corrupt/foreign recovery scope without deleting the original recovery record',()=>{
    const storage=new Storage();const journal=new RepositorySaveJournal('actor','repo:a',storage);
    const wrong=beginRepositorySave('actor','update',configuration,{},'b');journal.write(wrong);
    expect(()=>journal.read()).toThrow('invalid');expect(storage.values.size).toBe(1);
  });
  it('keeps secrets and runtime metadata out of public drafts and rejects embedded URL credentials',()=>{
    const value=publicConfiguration({...configuration,...secrets,total_errors:99} as PublicConfiguration);
    expect(value).toEqual(configuration);expect(()=>publicConfiguration({svn_url:'https://user:password@example.invalid/trunk'})).toThrow();
    expect(()=>publicConfiguration({git_api_url:'https://example.invalid/api?token=private'})).toThrow();
    expect(publicConfiguration({svn_url:'https://example.invalid/path@literal'}).svn_url).toBe('https://example.invalid/path@literal');
  });
});

it('#115 latches unsupported/missing confirmations instead of registering a second repository on retry',async()=>{
  const f=fixture();f.transport.configuration=async()=>{f.state.configCalls++;return {id:'known-legacy-created-id'} as Repository;};
  await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow('did not match');
  const saved=f.journal.read()!;expect(saved.repoId).toBe('known-legacy-created-id');expect(saved.protocolFailure).toBe('configuration');
  await expect(runRepositorySave(saved,secrets,f.journal,f.transport)).rejects.toThrow('No write will be blindly retried');
  expect(f.state.configCalls).toBe(1);expect(f.state.credentialCalls).toBe(0);
});

it('#115 retrying a configuration failure includes newly entered credentials instead of falsely closing',async()=>{
  const f=fixture();const first=beginRepositorySave('actor','create',configuration,{});f.state.configFail=true;
  await expect(runRepositorySave(first,{},f.journal,f.transport)).rejects.toThrow();
  const next=prepareRepositorySaveAttempt(f.journal.read(),configuration,{git_token:'newly-entered'},{owner:'actor'});
  expect(next.attempt.configRequestId).toBe(first.configRequestId);expect(next.attempt.credentialFields).toEqual(['git_token']);
  f.state.configFail=false;const final=await runRepositorySave(next.attempt,{git_token:'newly-entered'},f.journal,f.transport);
  expect(repositorySaveComplete(final)).toBe(true);expect(f.state.credentialEffects).toBe(1);expect(f.state.secretValue).toContain('newly-entered');
});

it('#115 reload after a lost create response cannot forget the originally required credential stage',async()=>{
  const f=fixture();f.state.loseConfig=true;await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow();
  const next=prepareRepositorySaveAttempt(f.journal.read(),configuration,{}, {owner:'actor'});
  expect(next.attempt.credentialFields).toEqual(['svn_password','git_token']);
  await expect(runRepositorySave(next.attempt,{},f.journal,f.transport)).rejects.toThrow('Re-enter');
  expect(f.journal.read()?.repoId).toBe('repo-created');expect(f.state.configEffects).toBe(1);expect(f.state.credentialEffects).toBe(0);
});

it('#115 newest public edits replace older drafts and survive subsequent progress writes',async()=>{
  const f=fixture();f.journal.write(f.attempt);
  f.journal.rememberDraft({...configuration,name:'first edit'});
  f.journal.rememberDraft({...configuration,name:'latest edit'});
  await runRepositorySave(f.attempt,secrets,f.journal,f.transport);
  expect(f.journal.read()?.draftConfiguration?.name).toBe('latest edit');
});

it('#115 a failed confirmation read cannot turn an already submitted snapshot into a different request body',async()=>{
  const f=fixture();f.state.credentialFail=true;await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow();
  const known=f.journal.read()!;known.configurationStage='unknown';
  const prepared=prepareRepositorySaveAttempt(known,{...configuration,name:'later unsaved edit'}, {}, {owner:'actor'});
  expect(prepared.attempt.configuration).toEqual(configuration);expect(prepared.replacePrivateInputs).toBe(false);
  expect(prepared.attempt.credentialFields).toEqual(['svn_password','git_token']);
});

it('#115 partial re-entry cannot be falsely confirmed by an earlier credential receipt',async()=>{
  const f=fixture();f.state.loseCredentials=true;await expect(runRepositorySave(f.attempt,secrets,f.journal,f.transport)).rejects.toThrow();
  await expect(runRepositorySave(f.journal.read()!,{git_token:'newly-entered-different-token'},f.journal,f.transport)).rejects.toThrow('every originally required');
  expect(f.state.credentialEffects).toBe(1);expect(f.journal.read()?.credentialStage).toBe('unknown');
});

it('#115 only actually submitted credential fields count as confirmed form values',()=>{
  const attempt=beginRepositorySave('actor','update',configuration,{svn_password:'original'},'repo-a');
  expect(submittedCredentialSnapshot(attempt,{svn_password:'original',git_token:'new-unsaved'})).toEqual({svn_password:'original'});
  expect(submittedCredentialSnapshot(attempt,{git_token:'new-unsaved'})).toEqual({});
});

it('#115 a completed old snapshot clears only confirmed bytes and preserves newer secret edits',()=>{
  expect(retainUnsavedCredentials({svn_password:'confirmed',git_token:'newer-unsaved'},
    {svn_password:'confirmed',git_token:'previous-token'})).toEqual({svn_password:'',git_token:'newer-unsaved'});
});

describe('#115 whole-story review corrections',()=>{
  it.each([{}, {save_receipt:{request_id:'wrong'}}, {save_receipt:undefined}])('RS115-1 prepare/reload/retry preserves an ID-less malformed-response latch: %j',async malformed=>{
    const f=fixture();f.transport.configuration=async()=>{f.state.configCalls++;f.state.configEffects++;return malformed as Repository;};
    const first=prepareRepositorySaveAttempt(null,configuration,secrets,{owner:'actor'}).attempt;
    await expect(runRepositorySave(first,secrets,f.journal,f.transport)).rejects.toThrow('did not match');
    const saved=f.journal.read()!;expect(saved.repoId).toBeUndefined();expect(saved.protocolFailure).toBe('configuration');
    const retry=prepareRepositorySaveAttempt(saved,configuration,secrets,{owner:'actor'}).attempt;
    expect(retry.configRequestId).toBe(first.configRequestId);expect(retry.credentialRequestId).toBe(first.credentialRequestId);
    await expect(runRepositorySave(retry,secrets,f.journal,f.transport)).rejects.toThrow('No write will be blindly retried');
    expect(f.state.configEffects).toBe(1);expect(f.state.configCalls).toBe(1);expect(f.state.credentialCalls).toBe(0);
    expect(()=>beginReviewedRepositorySave(saved,{id:'unverified',updated_at:time},configuration,secrets,'actor')).toThrow('retained repository');
  });
  function versionedFixture(){
    const f=fixture(),t2='2026-11-03T18:01:00Z',t3='2026-11-03T18:02:00Z';
    const live={version:time,configuration:{...configuration,poll_interval_secs:99},credentialValue:'later-unrelated-secret',configEffects:0,credentialEffects:0};
    const known=new Map<string,RepositorySaveReceipt>();
    const transport:RepositorySaveTransport={
      configuration:async a=>{
        f.state.configCalls++;let prior=known.get(a.configRequestId);
        if(!prior){if(a.expectedUpdatedAt!==live.version)throw new ApiError(409,'settings changed');live.configuration={...live.configuration,...a.configuration};live.version=live.version===time ? t2:t3;live.configEffects++;prior={...receipt(a),configuration_updated_at:live.version};known.set(a.configRequestId,prior);}
        return {id:a.repoId,updated_at:live.version,save_receipt:prior} as Repository;
      },
      credentials:async(a,input)=>{if(a.configurationReceipt?.configuration_updated_at!==live.version)throw new ApiError(409,'settings changed before credentials');live.credentialEffects++;live.credentialValue=input.git_token!;return {ok:true,save_receipt:{...receipt(a,true),configuration_updated_at:live.version}};},
      lookup:async id=>{const found=known.get(id);if(!found)throw new ApiError(404,'not found');return found;},
    };
    return {...f,t2,t3,live,transport};
  }
  it('RS115-2 no-receipt stale configuration refuses Resume and supports explicit reviewed same-ID recovery',async()=>{
    const f=versionedFixture();f.live.version=f.t2;const input={git_token:'selected-new'};
    const old=beginRepositorySave('actor','update',configuration,input,'repo-a',time);
    await expect(runRepositorySave(old,input,f.journal,f.transport)).rejects.toThrow('settings changed');
    const conflict=f.journal.read()!;expect(conflict.configurationStage).toBe('conflict');expect(conflict.configurationReceipt).toBeUndefined();
    const resume=prepareRepositorySaveAttempt(conflict,configuration,input,{owner:'actor',targetId:'repo-a',expectedUpdatedAt:f.t2});
    await expect(runRepositorySave(resume.attempt,input,f.journal,f.transport)).rejects.toThrow('Load and review');expect(f.state.configCalls).toBe(1);expect(f.live.configEffects).toBe(0);
    const next=beginReviewedRepositorySave(conflict,{id:'repo-a',updated_at:f.live.version},{...f.live.configuration,name:'selected current snapshot'},input,'actor');
    expect(next.configRequestId).not.toBe(old.configRequestId);expect(next.credentialRequestId).not.toBe(old.credentialRequestId);
    const done=await runRepositorySave(next,input,f.journal,f.transport);expect(repositorySaveComplete(done)).toBe(true);expect(done.repoId).toBe('repo-a');
    expect(f.live.configuration.poll_interval_secs).toBe(99);expect(f.live.configuration.name).toBe('selected current snapshot');expect(f.live.configEffects).toBe(1);expect(f.live.credentialEffects).toBe(1);
    expect(f.journal.read()?.priorAttempts?.[0].configRequestId).toBe(old.configRequestId);expect(f.journal.read()?.priorAttempts?.[0].configurationStage).toBe('conflict');
  });
  it('RS115-2 later settings between configuration and credentials retain the old receipt and permit deliberate fresh recovery',async()=>{
    const f=versionedFixture(),input={git_token:'selected-new'};const old=beginRepositorySave('actor','update',configuration,input,'repo-a',time),original=f.transport.credentials;
    f.transport.credentials=async(a,i,signal)=>{f.live.version=f.t3;f.live.configuration.poll_interval_secs=777;return original(a,i,signal);};
    await expect(runRepositorySave(old,input,f.journal,f.transport)).rejects.toThrow('settings changed before credentials');
    const conflict=f.journal.read()!;expect(conflict.configurationStage).toBe('saved');expect(conflict.credentialStage).toBe('conflict');expect(conflict.configurationReceipt?.configuration_updated_at).toBe(f.t2);
    const resume=prepareRepositorySaveAttempt(conflict,configuration,input,{owner:'actor',targetId:'repo-a',expectedUpdatedAt:f.t3});
    await expect(runRepositorySave(resume.attempt,input,f.journal,f.transport)).rejects.toThrow('Load and review');expect(f.live.configEffects).toBe(1);expect(f.live.credentialEffects).toBe(0);expect(f.live.credentialValue).toBe('later-unrelated-secret');
    f.transport.credentials=original;const fresh={git_token:'explicitly-reentered'};
    const next=beginReviewedRepositorySave(conflict,{id:'repo-a',updated_at:f.live.version},f.live.configuration,fresh,'actor');
    const done=await runRepositorySave(next,fresh,f.journal,f.transport);expect(repositorySaveComplete(done)).toBe(true);expect(done.repoId).toBe('repo-a');expect(f.live.configuration.poll_interval_secs).toBe(777);expect(f.live.credentialValue).toBe(fresh.git_token);
    expect(f.journal.read()?.priorAttempts?.[0].configurationReceipt).toEqual(conflict.configurationReceipt);expect(f.journal.read()?.priorAttempts?.[0].credentialStage).toBe('conflict');
  });
  it('RS115-3 actual editor ownership blocks A-to-B rebinding and keeps both journals independent',async()=>{
    const f=fixture(),origin={owner:'actor',targetId:'A',session:'old'};let current={...origin};const owner=new RepositoryEditorOwnership(origin,()=>current);
    const a=new RepositorySaveJournal('actor','repo:A',f.storage),b=new RepositorySaveJournal('actor','repo:B',f.storage);
    a.write(beginRepositorySave('actor','update',configuration,secrets,'A',time));current={...current,targetId:'B'};
    expect(()=>owner.assertCurrent()).toThrow('context changed');expect(b.read()).toBeNull();expect(a.read()?.configuration).toEqual(configuration);
    expect(repositoryEditorKey(origin)).not.toBe(repositoryEditorKey(current));
    const bOwner=new RepositoryEditorOwnership(current,()=>current),bInput={git_token:'B-only'};
    const transport:RepositorySaveTransport={...f.transport,configuration:async(s,signal)=>{bOwner.assertCurrent();expect(s.repoId).toBe('B');expect(s.configuration.name).toBe('B only');return f.transport.configuration(s,signal);},credentials:async(s,i,signal)=>{bOwner.assertCurrent();expect(i).toEqual(bInput);return f.transport.credentials(s,i,signal);}};
    await runRepositorySave(beginRepositorySave('actor','update',{...configuration,name:'B only'},bInput,'B',time),bInput,b,transport);
    expect(a.read()?.repoId).toBe('A');expect(b.read()?.repoId).toBe('B');
  });
  it.each(['route','actor','session'] as const)('RS115-3 pending old callbacks/effects cannot advance a replacement %s view',async change=>{
    const f=fixture();let current={owner:'actor',targetId:'A',session:'old'};const owner=new RepositoryEditorOwnership({...current},()=>current);let callbacks=0,oldErrors=0,release!:()=>void;
    const gate=new Promise<void>(resolve=>{release=resolve;}),configurationCall=f.transport.configuration,credentialCall=f.transport.credentials;
    f.transport.configuration=async(a,signal)=>{owner.assertCurrent();await gate;return configurationCall(a,signal);};f.transport.credentials=async(...args)=>{owner.assertCurrent();return credentialCall(...args);};
    const running=runRepositorySave(f.attempt,secrets,f.journal,f.transport,()=>{if(owner.isCurrent())callbacks++;}).catch(error=>{if(owner.isCurrent())oldErrors++;throw error;});const before=callbacks;
    current=change==='route' ? {...current,targetId:'B'} : change==='actor' ? {...current,owner:'other'} : {...current,session:'new'};owner.retire();release();
    await expect(running).rejects.toThrow('context changed');expect(callbacks).toBe(before);expect(oldErrors).toBe(0);expect(f.state.credentialCalls).toBe(0);expect(f.journal.read()?.repoId).toBe('repo-created');expect(f.journal.read()?.credentialStage).toBe('unknown');
  });
  const changedPreferences=[{importMode:'snapshot',svnRevision:'HEAD'},{importMode:'full_history',svnRevision:'42'},{importMode:'snapshot',svnRevision:'42'}];
  it.each(changedPreferences.flatMap(preferences=>[false,true].map(credentialsDelayed=>({preferences,credentialsDelayed}))))('RS115-4 preference-only edits survive delayed stages and reopen: %j',async({preferences,credentialsDelayed})=>{
    const f=fixture();f.attempt.preferences={importMode:'full_history',svnRevision:'HEAD'};let release!:()=>void;const gate=new Promise<void>(resolve=>{release=resolve;});
    if(credentialsDelayed){const original=f.transport.credentials;f.transport.credentials=async(...args)=>{await gate;return original(...args);};}else f.state.gate=gate;
    const running=runRepositorySave(f.attempt,secrets,f.journal,f.transport);if(credentialsDelayed)await new Promise(resolve=>setTimeout(resolve,0));
    f.journal.rememberDraft(configuration,preferences);const reopened=new RepositorySaveJournal('actor','create',f.storage);expect(reopened.read()?.draftPreferences).toEqual(preferences);
    release();const done=await running;expect(sameImportPreferences(preferences,done.preferences)).toBe(false);expect(reopened.read()?.draftPreferences).toEqual(preferences);
    const effects=[f.state.configEffects,f.state.credentialEffects];expect(canContinueWithPreferences(done,configuration,{})).toBe(true);
    expect({repoId:done.repoId,preferences:reopened.read()?.draftPreferences}).toEqual({repoId:'repo-created',preferences});
    expect([f.state.configEffects,f.state.credentialEffects]).toEqual(effects);expect(done.configRequestId).toBe(f.attempt.configRequestId);expect(canContinueWithPreferences(done,{...configuration,name:'not just preferences'},{})).toBe(false);
  });
});
