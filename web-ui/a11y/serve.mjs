import { createServer as createViteServer } from 'vite';
import { startFixture } from './fixture.mjs';

// Manual keyboard/screen-reader qualification only. Never starts a RepoSync engine.
let fixture,vite,stopRequested=false;
async function closeOwned() {
 const ownVite=vite,ownFixture=fixture;vite=undefined;fixture=undefined;
 try { await ownVite?.close(); } finally { await ownFixture?.close(); }
}
async function stop() { stopRequested=true;clearTimeout(limit);await closeOwned(); }
const limit=setTimeout(()=>{console.log('Synthetic verification window expired.');void stop()},15*60*1000);
process.once('SIGINT',()=>void stop());process.once('SIGTERM',()=>void stop());
async function main() {
 try {
  fixture=await startFixture();if(stopRequested){await closeOwned();return;}
  vite=await createViteServer({server:{host:'127.0.0.1',port:39731,strictPort:true,proxy:{'/api':fixture.origin,'/ws':{target:fixture.origin,ws:false}}}});if(stopRequested){await closeOwned();return;}
  await vite.listen();if(stopRequested){await closeOwned();return;}
  console.log(JSON.stringify({purpose:'RepoSync #121 synthetic manual accessibility verification only',ui:'http://127.0.0.1:39731',fixture:fixture.origin,user:'fixture-admin',password:'synthetic-only',maximum_minutes:15,stop:'Ctrl-C',effects:'in-memory fixture responses only; no real accounts/repos, credentials or engine'}));
 } catch(error){console.error(error);process.exitCode=1;await stop()}
}
await main();
