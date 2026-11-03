import { describe, expect, it } from 'vitest';
import { ApiError, type Repository, type RepositorySaveReceipt } from './api';
import {
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
