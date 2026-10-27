// Viewer-only opt-in. The trusted Mac preview never calls or serves this module.
export const privatePreview=location=>['127.0.0.1','localhost','[::1]'].includes(location.hostname);
export async function attachPeers(engine,{location=globalThis.location,signal,onMetrics=()=>{},fetchImpl=fetch,
  loadSdk=()=>import('./vendor/relayswarm-hls-swarm-0.4.4.mjs')}={}) {
  if(privatePreview(location))throw Error('Peer connections disabled in private preview');
  const response=await fetchImpl('/tv/swarm/config',{credentials:'omit',cache:'no-store',redirect:'error',signal:AbortSignal.any([signal,AbortSignal.timeout(4000)])});
  if(!response.ok)throw Error('Peer assistance unavailable');
  const data=await response.json();
  const {createHlsSwarm}=await loadSdk();if(signal.aborted)throw new DOMException('Stopped','AbortError');
  const relay=new URL('/tv/swarm/relay',location.href);relay.protocol=relay.protocol==='https:'?'wss:':'ws:';
  const swarm=createHlsSwarm({mode:'peer-first',swarmId:data.swarmId,streamEvent:data.streamEvent,
    relays:[relay.href],iceServers:[{urls:'stun:stun.cloudflare.com:3478'}],
    originFallbackMs:200,maxPeerWaitMs:500,originReserveMs:3000,sustainablePeerWait:true,
    maxPeers:4,maxUploadPeers:2,maxUploadBytesPerSecond:250000,maxUploadBytesPerSession:128*1024*1024,
    maxHeldBytes:16*1024*1024,maxSegmentBytes:8*1024*1024,serve:true,metricsIntervalMs:1000,
    coreWasm:'/tv/vendor/relayswarm-core-0.4.4.wasm',
    async getCommitment(context,{signal}) {
      const frag=context.frag,url=new URL(context.url),playlist=new URL(frag.baseurl);
      if(url.origin!==location.origin||playlist.origin!==location.origin)throw Error('Foreign commitment context');
      const query=new URLSearchParams({path:url.pathname,sequence:String(frag.sn),discontinuity:String(frag.cc),level:String(frag.level),playlist:playlist.pathname,mediaType:frag.type});
      const response=await fetchImpl(`/tv/swarm/commitment?${query}`,{credentials:'omit',cache:'no-store',redirect:'error',signal:AbortSignal.any([signal,AbortSignal.timeout(3000)])});
      if(!response.ok)throw Error('Commitment unavailable');return response.json();
    },onMetrics});
  const disabled=swarm.metrics().disabledReason;if(disabled){swarm.stop();throw Error('Peer assistance unsupported');}
  signal.addEventListener('abort',()=>swarm.stop(),{once:true});swarm.attach(engine);
  return {stop:()=>swarm.stop(),stream:'/media/index.m3u8'};
}
