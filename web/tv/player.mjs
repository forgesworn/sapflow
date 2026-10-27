import { createConnectionMonitor } from './connection.mjs';
import { createTestTone } from './tone.mjs';
import {createControls} from './controls.mjs';
import {createNetwork} from './network.mjs';
import {createAudience} from './audience.mjs';
import {createIdent} from './ident.mjs';
import {PLAYER_VERSION,newerPlayerVersion} from './version.mjs';
const STREAM = '/tv/live/event-tv/index.m3u8';

/** Origin playback with explicit, viewer-only peer assistance. */
export function createTvPlayer({ video, watch, edge, status, indicator, placeholder,
  fetchImpl = globalThis.fetch.bind(globalThis), loadEngine = () => import('./cmaf-player.mjs'),
  schedule = setTimeout, cancel = clearTimeout,
  info = {}, standbyTitle=null, tone=null, peers=null, peerStatus=null,
  onState=()=>{}, onMetrics=()=>{}, onRecovery=()=>{}, onInfo=()=>{},
  locationImpl=globalThis.location, loadPeers=()=>import('./peers.mjs'), preferNative = globalThis.navigator?.vendor === 'Apple Computer, Inc.' } = {}) {
  let watching = false, stopped = false, generation = 0, hls = null, request = null;
  let peerSession=null,peerFailed=false,userPaused=false,channelState=null,draining=false,originBytes=0;
  const privatePreview=['127.0.0.1','localhost','[::1]'].includes(locationImpl?.hostname);
  if(peers&&privatePreview){peers.checked=false;peers.disabled=true;}
  const peerInfo=value=>{if(peerStatus)peerStatus.textContent=value;};
  peerInfo(privatePreview?'Disabled in private Mac preview':'Off. Enable peer assistance to share with other viewers.');
  let retry = null, delay = 1000, startupTimer = null;
  const listeners = [];
  const field = (name, value) => { if (info[name]) info[name].textContent = value; };
  function updateInfo() {
    field('picture', video.videoWidth && video.videoHeight ? `${video.videoWidth} × ${video.videoHeight}` : 'Waiting for playback');
    let ahead = 0;
    for (let i = 0; i < (video.buffered?.length ?? 0); i++) {
      if (video.buffered.start(i) <= video.currentTime && video.currentTime <= video.buffered.end(i)) ahead = video.buffered.end(i) - video.currentTime;
    }
    field('buffer', watching ? `${ahead.toFixed(1)} seconds` : 'Not playing');
    field('delay', watching && Number.isFinite(hls?.latency) && hls.latency > 0 ? `${hls.latency.toFixed(1)} seconds (estimated)` : 'Not reported');
    onInfo({ahead,latency:hls?.latency??0,watching});
    if(draining&&watching&&!video.paused&&video.buffered?.length&&video.currentTime>=video.buffered.end(video.buffered.length-1)-.15)finishEnded();
  }
  const on = (target, name, fn) => { target.addEventListener(name, fn); listeners.push(() => target.removeEventListener(name, fn)); };
  function show(state, message) {
    tone?.setEnabled(['idle','stopped','offline','ended'].includes(state));
    if (standbyTitle) standbyTitle.textContent=({idle:'Ready',stopped:'You stopped watching',available:'Broadcast back on air',network:'Connection changed',connection:'Connection interrupted',connecting:'Tuning in',offline:'OFF AIR',error:'Unavailable',ended:'Broadcast ended',paused:'Paused',interrupted:'Broadcast interrupted'})[state] ?? '';
    indicator.dataset.state = state;
    indicator.textContent = ({ idle: 'Ready',stopped:'Stopped',available:'On air',network:'Connection changed',connection:'Connection interrupted', connecting: 'Connecting', offline: 'Off air', playing: 'Live', buffering: 'Buffering', error: 'Unavailable', ended:'Ended', paused:'Paused',interrupted:'Reconnecting' })[state];
    status.textContent = message;
    placeholder.hidden = ['playing','buffering','paused'].includes(state);
    if (state !== 'connecting') placeholder.dataset.kind = 'standby';
    edge.disabled = !['playing','buffering','paused'].includes(state);
    onState(state,{watching,active:state==='playing',peers:!!peerSession});
  }
  function clearMedia() {
    request?.abort(); request = null;
    if (retry !== null) cancel(retry);
    if (startupTimer !== null) cancel(startupTimer);
    retry = startupTimer = null;
    peerSession?.stop();peerSession=null;
    hls?.destroy(); hls = null;
    video.pause(); video.removeAttribute('src'); video.load();
  }
  function recover(token,reason="Receiver or connection unavailable") {
    if (!watching || stopped || token !== generation) return;
    if(channelState==='ended'||draining){drainEnded();return;}
    onRecovery(reason);generation++; clearMedia();
    show(channelState==='reconnecting'?'interrupted':'offline',channelState==='reconnecting'?'No new video is arriving. Waiting for the broadcaster to return or the receiver to confirm the end.':'The channel is off air or reconnecting. We’ll keep checking.');
    retry = schedule(() => { retry = null; void connect(); }, delay);
    delay = Math.min(delay * 2, 10000);
  }
  async function play(token) {
    if (token !== generation || !watching || stopped || userPaused) return;
    try { await video.play(); }
    catch {
      if (token === generation && watching) {
        if (startupTimer !== null) cancel(startupTimer);
        startupTimer = null;
        show('paused', 'Press play to start watching.');
      }
    }
  }
  async function connect() {
    if (!watching || stopped) return;
    if(channelState==='ended'){finishEnded();return;}
    const token = ++generation;
    clearMedia(); placeholder.dataset.kind='standby'; show('connecting', 'Tuning in…');
    request = new AbortController();
    startupTimer = schedule(() => recover(token,'No playable media within 25 seconds'), 25000);
    try {
      const response = await fetchImpl(STREAM, { cache: 'no-store', credentials: 'omit', signal: request.signal });
      if (token !== generation || !watching || stopped) return;
      if (!response.ok) { recover(token,`Receiver returned HTTP ${response.status??"unavailable"}`); return; }
      placeholder.dataset.kind='streaming';
      if (!peers?.checked && preferNative && video.canPlayType('application/vnd.apple.mpegurl')) {
        video.src = STREAM;
        await play(token);
      } else {
        const { default: Engine } = await loadEngine();
        if (token !== generation || !watching || stopped) return;
        if (!Engine.isSupported()) {
          if (video.canPlayType('application/vnd.apple.mpegurl')) { video.src = STREAM; await play(token); return; }
          stop(); show('error', 'This browser cannot play the live channel.'); return;
        }
        hls = new Engine({ lowLatencyMode: false, liveSyncDuration: 20, liveMaxLatencyDuration: 40,
          backBufferLength: 10, maxBufferLength: 30, maxMaxBufferLength: 40, enableWorker: false });
        hls.on(Engine.Events.MANIFEST_PARSED, (_event, detail = {}) => {
          if (token !== generation || !watching) return;
          const level = detail.levels?.[0];
          field('rate', level?.bitrate > 0 ? `${(level.bitrate / 1000000).toFixed(2)} Mbit/s` : 'Not advertised');
          field('codecs', [level?.videoCodec, level?.audioCodec].filter(Boolean).join(', ') || 'Not advertised');
          void play(token);
        });
        hls.on(Engine.Events.ERROR, (_event, detail) => { if (detail.fatal) {if(peerSession)peerFailed=true;recover(token,String(detail.details??'Media load failed'));} });
        hls.on(Engine.Events.FRAG_LOADED,(_event,detail)=>{if(token===generation&&!peerSession){originBytes+=detail.payload?.byteLength??0;onMetrics({bytes:{fromOrigin:originBytes,fromPeers:0,uploaded:0},peers:{connected:0}},false);}});
        let stream=STREAM;
        if(peers?.checked&&!privatePreview&&!peerFailed) {
          try {
            const {attachPeers}=await loadPeers();
            if(token!==generation||!watching)return;
            const session=await attachPeers(hls,{location:locationImpl,signal:request.signal,onMetrics:metrics=>{
              if(token!==generation)return;
              onMetrics(metrics,true);
              peerInfo(metrics.disabledReason?'Unavailable; using receiver':`${metrics.peers?.connected??0} peers · ${((metrics.bytes?.fromPeers??0)/1048576).toFixed(1)} MiB received · ${((metrics.bytes?.uploaded??0)/1048576).toFixed(1)} MiB shared`);
            }});
            if(token!==generation||!watching){session.stop();return;}
            peerSession=session;stream=session.stream;
          }catch {if(token!==generation||!watching)return;peerInfo('Peer assistance unavailable; using receiver.');}
        }
        hls.loadSource(stream); hls.attachMedia(video);
      }
    } catch { recover(token,'Receiver request failed'); }
  }
  function stop() {
    watching = false; generation++; clearMedia(); tone?.stop(); delay = 1000;peerFailed=false;userPaused=false;draining=false;originBytes=0;onMetrics({},false);
    watch.textContent = 'Watch live';
    peerInfo(privatePreview?'Disabled in private Mac preview':'Off. Peer connections closed.');
    show('stopped','You stopped watching. Press play to rejoin the broadcast.');
    field('rate', 'Not yet reported'); field('codecs', 'Not yet reported'); updateInfo();
  }
  function start(){if(channelState==='ended')channelState=null;userPaused=false;draining=false;watching=true;watch.textContent='Stop watching';void connect();}
  function finishEnded(){
    if(!watching&&indicator.dataset.state==='ended')return;
    watching=false;generation++;clearMedia();draining=false;userPaused=false;
    watch.textContent='Watch live';onMetrics({},false);
    show('ended','The broadcast has ended. Come back for the next programme.');updateInfo();
  }
  function drainEnded(){
    draining=true;if(startupTimer!==null)cancel(startupTimer);startupTimer=null;
    if(retry!==null)cancel(retry);retry=null;
    if(video.readyState>0&&video.buffered?.length){hls?.finish?.();updateInfo();}
    else finishEnded();
  }
  function togglePlayback(){
    if(!watching){start();return;}
    if(!video.paused){userPaused=true;video.pause();show('paused','Paused. Press play to continue.');}
    else{userPaused=false;void play(generation);}
  }
  on(watch,'click',()=>{if(watching)stop();else start();});
  if(peers)on(peers,'change',()=>{peerFailed=false;if(watching)void connect();});
  on(edge, 'click', () => {
    const position = hls?.liveSyncPosition;
    if (Number.isFinite(position)) video.currentTime = position;
    else if (video.seekable.length) video.currentTime = Math.max(video.seekable.start(video.seekable.length - 1), video.seekable.end(video.seekable.length - 1) - 20);
    void play(generation);
  });
  for (const name of ['loadedmetadata', 'timeupdate', 'progress', 'emptied']) on(video, name, updateInfo);
  on(video, 'playing', () => {
    if (!watching || stopped) return;
    if (startupTimer !== null) cancel(startupTimer);
    startupTimer = null; delay = 1000;userPaused=false;
    show('playing', 'You’re watching live.');
  });
  on(video, 'waiting', () => { if(watching&&draining){updateInfo();return;}if (watching && !userPaused && video.readyState > 0) show(channelState==='reconnecting'?'interrupted':channelState==='unknown'?'connection':'buffering',channelState==='reconnecting'?'No new video is arriving. Waiting for the broadcaster to return or the receiver to confirm the end.':channelState==='unknown'?'Your connection to the receiver was interrupted. Retrying.':'Catching up with the broadcast…'); });
  on(video, 'error', () => { if (watching && video.currentSrc) recover(generation,'Browser media decode error'); });
  on(video, 'pause', () => {if(watching&&video.readyState>0&&!video.ended&&['playing','buffering'].includes(indicator.dataset.state)){userPaused=true;show('paused','Paused. Press play to continue.');}});
  on(video, 'ended', () => { if(watching)finishEnded(); });
  return { stop, togglePlayback, networkChanged(){
    if(peerSession||peers?.checked){stop();if(peers)peers.checked=false;peerInfo('Peer assistance switched off after a network address change. Check your VPN before enabling it again.');show('network','Your visible network address changed. Peer assistance is off. Check your VPN, then press play to rejoin.');}
  }, get watching(){return watching;}, setChannelState(value){channelState=value;if(value==='unknown'&&watching&&indicator.dataset.state==='interrupted')show('connection','Your connection to the receiver was interrupted. Checking channel status again.');if(value==='reconnecting'&&watching&&['buffering','offline'].includes(indicator.dataset.state))show('interrupted','No new video is arriving. Waiting for the broadcaster to return or the receiver to confirm the end.');if(value==='ended'){if(watching)drainEnded();else show('ended','The broadcast has ended. Come back for the next programme.');}else if(!watching&&indicator.dataset.state==='ended'&&value==='live')show('available','The broadcast is back on air. Press play to tune in.');}, destroy() { stop(); stopped = true; tone?.destroy(); for (const remove of listeners) remove(); } };
}

if (typeof document !== 'undefined') {
  const get=id=>document.querySelector('#'+id),video=get('video'),network=createNetwork(document),ident=createIdent(get('loading-ident'));
  let player,controls,recoveries=0;
  const audience=createAudience({onSnapshot(data){
    get('audience-count').textContent=data?String(data.activePlayers):'—';get('audience-peers').textContent=data?String(data.peerEnabledPlayers):'—';
    if(!data){get('channel-state').textContent='Channel status unavailable';get('channel-state').dataset.state='unknown';player?.setChannelState('unknown');return;}
    const channel=data.channel?.state;get('channel-state').textContent=({live:'On air',offair:'Off air',reconnecting:'Reconnecting',ended:'Broadcast ended'})[channel]??'Checking channel';
    get('channel-state').dataset.state=channel??'unknown';player?.setChannelState(channel);
    get('player-update').hidden=!newerPlayerVersion(data.playerVersion);
  }});
  get('player-version').textContent='Sapflow '+PLAYER_VERSION;
  player = createTvPlayer({video,watch:get('watch'),edge:get('live-edge'),status:get('status'),indicator:get('indicator'),placeholder:get('placeholder'),peers:get('peer-assist'),peerStatus:get('peer-status'),standbyTitle:get('standby-title'),tone:createTestTone(get('test-tone')),
    info:Object.fromEntries(['picture','rate','codecs','buffer','delay'].map(key=>[key,get('media-'+key)])),
    onMetrics:(metrics,enabled)=>network.update(metrics,enabled),
    onRecovery(reason){get('last-recovery').textContent=reason;get('recovery-count').textContent=String(++recoveries);},
    onInfo({ahead,latency,watching}){get('buffer-fill').style.width=Math.min(100,ahead/20*100)+'%';get('playback-health').textContent=watching?ahead.toFixed(1)+'s buffered':'No media loaded';get('playback-position').textContent=watching?(latency>0?latency.toFixed(0)+'s behind live':'Live programme'):'Ready to tune in';get('quality-summary').textContent=video.getVideoPlaybackQuality?String(video.getVideoPlaybackQuality().droppedVideoFrames)+' dropped frames':'Not reported';},
    onState(state,presence){controls?.setState(state);ident.setState(state);audience.set(presence);get('standby-description').textContent=({connecting:'Preparing your picture. The stream starts when enough video is ready.',offline:'Waiting for the broadcaster to reconnect.',interrupted:'No new video is arriving. Waiting for reconnection or confirmation that the broadcast has ended.',ended:'Thanks for watching. Join us for the next broadcast.',idle:'Your front row to the live channel.',available:'A new broadcast is live. Press play to tune in.',connection:'Reconnecting to the receiver. Channel status is being checked.',network:'Check your VPN before rejoining. Peer assistance has been switched off.',stopped:'Press play to rejoin the broadcast.'})[state]??'';}
  });
  controls=createControls({video,screen:get('screen'),onToggle:()=>player.togglePlayback()});controls.setState('idle');ident.setState('idle');network.reset();audience.refresh();
  const connection=createConnectionMonitor({document,location,onAddressChange:()=>player.networkChanged()});
  addEventListener('pagehide',()=>{connection.destroy();audience.destroy();player.destroy();controls.destroy();ident.destroy();},{once:true});
}
