import {PLAYER_VERSION} from './version.mjs';
const privatePreview=location=>['127.0.0.1','localhost','[::1]'].includes(location.hostname);
export function createAudience({location=globalThis.location,fetchImpl=fetch,onSnapshot=()=>{},schedule=setTimeout,cancel=clearTimeout}={}){
 const local=privatePreview(location),id=local?null:Array.from(crypto.getRandomValues(new Uint8Array(16)),v=>v.toString(16).padStart(2,'0')).join('');
 let watching=false,active=false,peers=false,timer=null,controller=null,closed=false;
 async function request(report=false){
  controller?.abort();controller=new AbortController();
  try{
   const options={credentials:'omit',cache:'no-store',redirect:'error',signal:AbortSignal.any([controller.signal,AbortSignal.timeout(5000)])};
   if(report&&!local){options.method='POST';options.headers={'Content-Type':'application/json'};options.body=JSON.stringify({id,active,peers,version:PLAYER_VERSION});}
   const r=await fetchImpl('/tv/audience',options);if(!r.ok)throw Error('Unavailable');const data=await r.json();if(!closed)onSnapshot(data);
  }catch{if(!closed)onSnapshot(null);}
 }
 function later(){if(timer!==null)cancel(timer);timer=null;if(watching&&!closed)timer=schedule(async()=>{timer=null;await request(true);later();},10000);}
 return {refresh(){if(!closed)void request(false);},set({watching:nextWatching,active:nextActive,peers:nextPeers}){
  if(closed)return;
  const changed=watching!==nextWatching||active!==nextActive||peers!==nextPeers;watching=nextWatching;active=nextActive;peers=nextPeers;
  if(changed){void request(true);later();}
 },destroy(){closed=true;if(timer!==null)cancel(timer);controller?.abort();if(!local&&active)void fetchImpl('/tv/audience',{method:'POST',credentials:'omit',keepalive:true,headers:{'Content-Type':'application/json'},body:JSON.stringify({id,active:false,peers:false,version:PLAYER_VERSION})}).catch(()=>{});}};
}
