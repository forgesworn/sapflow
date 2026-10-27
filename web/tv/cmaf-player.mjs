// Sapflow's complete-fragment MediaSource player. MIT, no transmuxer dependency.
import {parseMaster,parseMedia} from './playlist.mjs';

export async function boundedBytes(response,limit) {
  if(!response.ok)throw Object.assign(Error(`Media HTTP ${response.status}`),{status:response.status});
  const reader=response.body.getReader(),chunks=[];let size=0;
  try {while(true){const {done,value}=await reader.read();if(done)break;size+=value.length;if(size>limit)throw Error('Media byte budget exceeded');chunks.push(value);}}
  catch(error){await reader.cancel().catch(()=>{});throw error;}
  finally {reader.releaseLock();}
  const output=new Uint8Array(size);let offset=0;for(const chunk of chunks){output.set(chunk,offset);offset+=chunk.length;}return output;
}
// Retry complete reads after a route change; never append partial media.
async function retryBytes(url,{signal,limit,retry404=false,attemptMs=0,onAttempt=()=>{},onResponse=()=>{}}){
 for(let attempt=0;attempt<5;attempt++){
  signal.throwIfAborted();onAttempt(attempt);
  // A hung attempt is retried; only the caller's signal ends the read.
  const attemptSignal=attemptMs?AbortSignal.any([signal,AbortSignal.timeout(attemptMs)]):signal;
  try{const response=await fetch(url,{signal:attemptSignal,redirect:'error',cache:'no-store',credentials:'omit'});onResponse(response);
   if(!response.ok){await response.body?.cancel();throw Object.assign(Error(`Media HTTP ${response.status}`),{status:response.status});}
   return await boundedBytes(response,limit);
  }catch(error){
   if(signal.aborted||attempt===4||!(error instanceof TypeError||error?.name==='TimeoutError'||[429,502,503,504,...(retry404?[404]:[])].includes(error.status)))throw error;
   await new Promise(resolve=>{const done=()=>{clearTimeout(timer);signal.removeEventListener('abort',done);resolve();};const timer=setTimeout(done,[500,1500,3000,5000][attempt]);signal.addEventListener('abort',done,{once:true});if(signal.aborted)done();});
  }
 }
}
export function channelUrl(value,base) {
  const url=new URL(value,base);
  if(url.origin!==new URL(base).origin||url.username||url.password||url.search||url.hash||!['/tv/live/event-tv/index.m3u8','/media/index.m3u8'].includes(url.pathname))throw Error('Untrusted channel URL');
  return url.href;
}
// Nudge at most two seconds of unbuffered time only when decoding is waiting.
export function gapPosition(ranges,time,readyState,seeking=false) {
  if(readyState>=3||seeking)return null;
  for(let i=1;i<ranges.length;i++)if(time>=ranges.end(i-1)-.1&&time<ranges.start(i)&&ranges.start(i)-time<=2)return ranges.start(i)+.001;
  return null;
}
export function mediaSourceType(environment=globalThis,mime=null){
 return [environment.MediaSource,environment.ManagedMediaSource].find(Type=>typeof Type==='function'&&(!mime||Type.isTypeSupported(mime)))??null;
}
export class FetchLoader {
  constructor(){this.stats={aborted:false,loaded:0,total:0,retry:0,chunkCount:0,bwEstimate:0,loading:{start:0,first:0,end:0},parsing:{start:0,end:0},buffering:{start:0,first:0,end:0}};this.controller=null;}
  load(context,config,callbacks) {
    this.context=context;const controller=new AbortController();this.controller=controller;this.stats.aborted=false;const started=performance.now();this.stats.loading.start=started;
    const timer=setTimeout(()=>controller.abort(),config?.timeout??15000);
    let received;
    // A hung attempt is retried inside the overall deadline instead of consuming it.
    retryBytes(context.url,{signal:controller.signal,limit:8*1024*1024,attemptMs:config?.attemptTimeout??7000,onAttempt:attempt=>{this.stats.retry=attempt;},onResponse:response=>{received=response;this.stats.loading.first=performance.now();}}).then(data=>{
      this.stats.loading.end=performance.now();this.stats.loaded=this.stats.total=data.byteLength;
      this.stats.bwEstimate=data.byteLength*8000/Math.max(1,this.stats.loading.end-started);
      if(this.controller===controller&&!this.stats.aborted)callbacks.onSuccess({url:context.url,data:data.buffer},this.stats,context,{getResponseHeader:name=>received.headers.get(name)});
    }).catch(error=>{if(this.controller===controller&&!this.stats.aborted)callbacks.onError({code:0,text:error.message},context,null,this.stats);}).finally(()=>clearTimeout(timer));
  }
  abort(){if(!this.stats.loading.end)this.stats.aborted=true;this.controller?.abort();}
  destroy(){this.abort();}
}
function waitEvent(target,event,signal,action) {
  return new Promise((resolve,reject)=>{
    const cleanup=()=>{target.removeEventListener(event,done);target.removeEventListener('error',fail);signal.removeEventListener('abort',abort);};
    const done=()=>{cleanup();resolve();},fail=()=>{cleanup();reject(Error('MediaSource rejected media'));},abort=()=>{cleanup();reject(new DOMException('Stopped','AbortError'));};
    if(signal.aborted){abort();return;}
    target.addEventListener(event,done,{once:true});target.addEventListener('error',fail,{once:true});signal.addEventListener('abort',abort,{once:true});
    try{action?.();}catch(error){cleanup();reject(error);}
  });
}
export default class CmafPlayer {
  static Events={MANIFEST_PARSED:'hlsManifestParsed',ERROR:'hlsError',FRAG_LOADING:'hlsFragLoading',FRAG_LOADED:'hlsFragLoaded',FRAG_BUFFERED:'hlsFragBuffered',LEVEL_LOADED:'hlsLevelLoaded',LEVEL_UPDATED:'hlsLevelUpdated'};
  static isSupported(){return mediaSourceType()!==null;}
  constructor(config={}) {
    this.config={loader:FetchLoader,fLoader:undefined,progressive:false,liveSyncDuration:20,...config};
    this.listeners=new Map();this.controller=new AbortController();this.media=null;this.source=null;this.buffer=null;this.objectUrl=null;this.url=null;
    this.levels=[];this.currentLevel=this.loadLevel=0;this.latestLevelDetails=null;this.offset=null;this.edge=null;this.positions=new Map();this.initialised=false;this.started=false;this.loaders=new Set();this.onWaiting=()=>this.nudgeGap();
  }
  on(name,callback){if(!this.listeners.has(name))this.listeners.set(name,new Set());this.listeners.get(name).add(callback);}
  off(name,callback){this.listeners.get(name)?.delete(callback);}
  trigger(name,data){for(const callback of this.listeners.get(name)??[])callback(name,data);}
  loadSource(url){this.url=channelUrl(url,location.href);this.begin();}
  attachMedia(media){this.media?.removeEventListener('waiting',this.onWaiting);this.media=media;media.addEventListener('waiting',this.onWaiting);this.begin();}
  nudgeGap(){if(!this.initialised||this.controller.signal.aborted)return;const position=gapPosition(this.media.buffered,this.media.currentTime,this.media.readyState,this.media.seeking);if(position!==null)this.media.currentTime=position;}
  get liveSyncPosition(){return this.offset===null||this.edge===null?NaN:this.offset+this.edge-this.config.liveSyncDuration;}
  get latency(){return this.offset===null||this.edge===null?0:Math.max(0,this.offset+this.edge-this.media.currentTime);}
  async text(url){
    // Five 4-second attempts plus backoff stay inside the 20-30 second buffer.
    return new TextDecoder('ascii',{fatal:true}).decode(await retryBytes(url,{signal:this.controller.signal,limit:65536,retry404:true,attemptMs:4000}));
  }
  begin(){if(this.started||!this.media||!this.url)return;this.started=true;void this.run().catch(error=>{if(!this.controller.signal.aborted)this.trigger(CmafPlayer.Events.ERROR,{fatal:true,details:error.message});});}
  async append(bytes){if(this.buffer.updating)await waitEvent(this.buffer,'updateend',this.controller.signal);await waitEvent(this.buffer,'updateend',this.controller.signal,()=>this.buffer.appendBuffer(bytes));}
  async download(record,playlistUrl,initialisation=false) {
    const url=new URL(initialisation?record.init:record.name,playlistUrl).href;
    const frag={url,sn:initialisation?'initSegment':record.sn,cc:record.cc,level:0,type:'main',start:this.offset===null?0:this.offset+record.position,duration:record.duration,baseurl:playlistUrl};
    const context={url,frag,responseType:'arraybuffer'};const Loader=this.config.fLoader??this.config.loader;const loader=new Loader(this.config);this.loaders.add(loader);frag.stats=loader.stats;
    this.trigger(CmafPlayer.Events.FRAG_LOADING,{frag});
    let response;try{response=await new Promise((resolve,reject)=>{
      const abort=()=>{loader.abort();reject(new DOMException('Stopped','AbortError'));};this.controller.signal.addEventListener('abort',abort,{once:true});
      const finish=fn=>(...args)=>{this.controller.signal.removeEventListener('abort',abort);fn(...args);};
      loader.load(context,{timeout:15000,loadPolicy:{maxLoadTimeMs:15000}}, {onSuccess:finish((data,stats)=>resolve({data:data.data,stats})),onError:finish(error=>reject(Error(error.text??'Fragment failed'))),onTimeout:finish(()=>reject(Error('Fragment timed out')))});
    });
    }catch(error){loader.destroy();this.loaders.delete(loader);throw error;}
    this.trigger(CmafPlayer.Events.FRAG_LOADED,{frag,stats:response.stats,payload:response.data});
    return {frag,loader,...response};
  }
  async appendDownloaded({frag,loader,data,stats}) {
    try{await this.append(data);this.nudgeGap();stats.buffering.end=performance.now();this.trigger(CmafPlayer.Events.FRAG_BUFFERED,{frag,stats});}
    finally{loader.destroy();this.loaders.delete(loader);}
  }
  async run() {
    const master=parseMaster(await this.text(this.url)),mime=`video/mp4; codecs="${master.codec}"`;
    const SourceType=mediaSourceType(globalThis,mime);if(!SourceType)throw Error('Channel codecs unsupported');
    this.managed=SourceType===globalThis.ManagedMediaSource;
    if(this.managed){this.previousRemotePlayback=this.media.disableRemotePlayback;this.media.disableRemotePlayback=true;}
    this.levels=[{bitrate:master.bitrate,averageBitrate:master.averageBitrate,videoCodec:master.codec.split(',')[0],audioCodec:master.codec.split(',')[1]}];
    this.source=new SourceType();this.objectUrl=URL.createObjectURL(this.source);this.media.src=this.objectUrl;
    await waitEvent(this.source,'sourceopen',this.controller.signal,()=>this.media.load());this.source.duration=Infinity;this.buffer=this.source.addSourceBuffer(mime);
    const playlistUrl=new URL(master.manifest,this.url).href;let next=null,lastInit=null,lastPrefix=null;
    while(!this.controller.signal.aborted) {
      const parsed=parseMedia(await this.text(playlistUrl)),segments=parsed.segments;
      const common=segments.find(s=>this.positions.has(s.name)&&!s.gap);let base=0;
      if(common){base=this.positions.get(common.name);for(const s of segments){if(s===common)break;base-=s.duration;}}
      this.positions=new Map();let position=base;
      for(const segment of segments){segment.position=position;this.positions.set(segment.name,position);position+=segment.duration;}
      this.edge=position;
      const first=segments[0],last=segments.at(-1);const prefix=last.init.slice(0,12);
      if(lastPrefix&&prefix!==lastPrefix)throw Error('Receiver stream restarted');lastPrefix=prefix;
      if(next===null||next<first.sn){let behind=0,index=segments.length-1;while(index>0&&behind<this.config.liveSyncDuration){behind+=segments[index].duration;index--;}next=segments[index].sn;}
      if(next>last.sn+1)throw Error('Receiver sequence restarted');
      this.latestLevelDetails={live:!parsed.end,edge:this.edge+(this.offset??0),startSN:first.sn,endSN:last.sn,fragments:segments};
      this.levels[0].details=this.latestLevelDetails;
      this.trigger(CmafPlayer.Events.LEVEL_UPDATED,{details:this.latestLevelDetails,level:0});
      for(let index=0;index<segments.length;) {
        const segment=segments[index];
        if(segment.sn<next){index++;continue;}
        if(this.managed&&this.initialised&&this.source.streaming===false)break;
        if(this.initialised&&this.buffer.buffered.length&&this.buffer.buffered.end(this.buffer.buffered.length-1)-this.media.currentTime>30)break;
        if(segment.gap){next=segment.sn+1;index++;continue;}
        if(segment.init!==lastInit){await this.appendDownloaded(await this.download(segment,playlistUrl,true));lastInit=segment.init;}
        // Hide Tor round trips behind at most four complete-fragment transfers.
        // Native appends remain ordered; no fragment crosses an init epoch.
        const batch=[];
        while(index<segments.length&&batch.length<4&&!segments[index].gap&&segments[index].init===lastInit&&segments[index].cc===segment.cc)batch.push(segments[index++]);
        const downloaded=batch.map(item=>this.download(item,playlistUrl).then(value=>({value}),error=>({error})));
        for(let item=0;item<batch.length;item++) {
          const segment=batch[item];next=segment.sn+1;
          const result=await downloaded[item];if(result.error)throw result.error;
          await this.appendDownloaded(result.value);
          if(this.offset===null&&this.buffer.buffered.length)this.offset=this.buffer.buffered.start(0)-segment.position;
          if(this.buffer.buffered.length) {
            const ranges=this.buffer.buffered,end=ranges.end(ranges.length-1),start=ranges.start(ranges.length-1);
            if(!this.initialised&&end-start>=Math.min(8,this.config.liveSyncDuration)) {
              this.initialised=true;this.media.currentTime=Math.max(start,Math.min(this.liveSyncPosition,end-.1));
              this.trigger(CmafPlayer.Events.MANIFEST_PARSED,{levels:this.levels});
            }
            if(this.initialised&&this.media.currentTime<ranges.start(0)-.1)this.media.currentTime=ranges.start(0);
            if(this.initialised&&this.liveSyncPosition-this.media.currentTime>12)this.media.currentTime=Math.max(start,this.liveSyncPosition);
            this.source.setLiveSeekableRange(ranges.start(0),Math.max(end,this.offset+this.edge));
            const keep=this.media.currentTime-15;
            if(keep>ranges.start(0)+5)await waitEvent(this.buffer,'updateend',this.controller.signal,()=>this.buffer.remove(0,keep));
          }
        }
      }
      if(parsed.end&&next>last.sn){
        if(!this.initialised&&this.buffer.buffered.length){this.initialised=true;this.media.currentTime=this.buffer.buffered.start(0);this.trigger(CmafPlayer.Events.MANIFEST_PARSED,{levels:this.levels});}
        this.source.endOfStream();return;
      }
      await new Promise(resolve=>{const timer=setTimeout(done,500);const signal=this.controller.signal;function done(){clearTimeout(timer);signal.removeEventListener('abort',done);resolve();}signal.addEventListener('abort',done,{once:true});});
    }
  }
  finish(){
    this.controller.abort();for(const loader of this.loaders)loader.destroy();this.loaders.clear();
    const end=()=>{try{if(this.source?.readyState==='open')this.source.endOfStream();}catch{}};
    if(this.buffer?.updating)this.buffer.addEventListener('updateend',end,{once:true});else end();
  }
  destroy(){if(this.managed&&this.media)this.media.disableRemotePlayback=this.previousRemotePlayback;this.media?.removeEventListener('waiting',this.onWaiting);this.controller.abort();for(const loader of this.loaders)loader.destroy();this.loaders.clear();try{if(this.buffer?.updating)this.buffer.abort();}catch{}if(this.objectUrl){if(this.media?.getAttribute('src')===this.objectUrl){this.media.pause();this.media.removeAttribute('src');this.media.load();}URL.revokeObjectURL(this.objectUrl);}this.listeners.clear();}
}
