const STREAM = '/tv/live/event-tv/index.m3u8';

/** Origin-only TV playback. No signalling, peer connections or remote assets. */
export function createTvPlayer({ video, watch, edge, status, indicator, placeholder,
  fetchImpl = globalThis.fetch.bind(globalThis), loadHls = () => import('./vendor/hls.light.min.mjs'),
  schedule = setTimeout, cancel = clearTimeout,
  info = {}, preferNative = globalThis.navigator?.vendor === 'Apple Computer, Inc.' } = {}) {
  let watching = false, stopped = false, generation = 0, hls = null, request = null;
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
  }
  const on = (target, name, fn) => { target.addEventListener(name, fn); listeners.push(() => target.removeEventListener(name, fn)); };
  function show(state, message) {
    indicator.dataset.state = state;
    indicator.textContent = ({ idle: 'Ready', connecting: 'Connecting', offline: 'Off air', playing: 'Live', buffering: 'Buffering', error: 'Unavailable' })[state];
    status.textContent = message;
    placeholder.hidden = state === 'playing' || state === 'buffering';
    edge.disabled = state !== 'playing' && state !== 'buffering';
  }
  function clearMedia() {
    request?.abort(); request = null;
    if (retry !== null) cancel(retry);
    if (startupTimer !== null) cancel(startupTimer);
    retry = startupTimer = null;
    hls?.destroy(); hls = null;
    video.pause(); video.removeAttribute('src'); video.load();
  }
  function recover(token) {
    if (!watching || stopped || token !== generation) return;
    generation++; clearMedia();
    show('offline', 'The channel is off air or reconnecting. We’ll keep checking.');
    retry = schedule(() => { retry = null; void connect(); }, delay);
    delay = Math.min(delay * 2, 10000);
  }
  async function play(token) {
    if (token !== generation || !watching || stopped) return;
    try { await video.play(); }
    catch {
      if (token === generation && watching) {
        if (startupTimer !== null) cancel(startupTimer);
        startupTimer = null;
        show('connecting', 'Press play on the video to start watching.');
      }
    }
  }
  async function connect() {
    if (!watching || stopped) return;
    const token = ++generation;
    clearMedia(); show('connecting', 'Tuning in…');
    request = new AbortController();
    startupTimer = schedule(() => recover(token), 25000);
    try {
      const response = await fetchImpl(STREAM, { cache: 'no-store', credentials: 'omit', signal: request.signal });
      if (token !== generation || !watching || stopped) return;
      if (!response.ok) { recover(token); return; }
      if (preferNative && video.canPlayType('application/vnd.apple.mpegurl')) {
        video.src = STREAM;
        await play(token);
      } else {
        const { default: Hls } = await loadHls();
        if (token !== generation || !watching || stopped) return;
        if (!Hls.isSupported()) {
          if (video.canPlayType('application/vnd.apple.mpegurl')) { video.src = STREAM; await play(token); return; }
          stop(); show('error', 'This browser cannot play the live channel.'); return;
        }
        hls = new Hls({ lowLatencyMode: false, liveSyncDuration: 10, liveMaxLatencyDuration: 20,
          backBufferLength: 10, maxBufferLength: 20, maxMaxBufferLength: 30, enableWorker: false });
        hls.on(Hls.Events.MANIFEST_PARSED, (_event, detail = {}) => {
          if (token !== generation || !watching) return;
          const level = detail.levels?.[0];
          field('rate', level?.bitrate > 0 ? `${(level.bitrate / 1000000).toFixed(2)} Mbit/s` : 'Not advertised');
          field('codecs', [level?.videoCodec, level?.audioCodec].filter(Boolean).join(', ') || 'Not advertised');
          void play(token);
        });
        hls.on(Hls.Events.ERROR, (_event, detail) => { if (detail.fatal) recover(token); });
        hls.loadSource(STREAM); hls.attachMedia(video);
      }
    } catch { recover(token); }
  }
  function stop() {
    watching = false; generation++; clearMedia(); delay = 1000;
    watch.textContent = 'Watch live';
    show('idle', 'Press Watch live to tune in.');
    field('rate', 'Not yet reported'); field('codecs', 'Not yet reported'); updateInfo();
  }
  on(watch, 'click', () => {
    if (watching) stop();
    else { watching = true; watch.textContent = 'Stop watching'; void connect(); }
  });
  on(edge, 'click', () => {
    const position = hls?.liveSyncPosition;
    if (Number.isFinite(position)) video.currentTime = position;
    else if (video.seekable.length) video.currentTime = Math.max(video.seekable.start(video.seekable.length - 1), video.seekable.end(video.seekable.length - 1) - 10);
    void play(generation);
  });
  for (const name of ['loadedmetadata', 'timeupdate', 'progress', 'emptied']) on(video, name, updateInfo);
  on(video, 'playing', () => {
    if (!watching || stopped) return;
    if (startupTimer !== null) cancel(startupTimer);
    startupTimer = null; delay = 1000;
    show('playing', 'You’re watching live.');
  });
  on(video, 'waiting', () => { if (watching && video.readyState > 0) show('buffering', 'Catching up with the broadcast…'); });
  on(video, 'error', () => { if (watching && video.currentSrc) recover(generation); });
  on(video, 'ended', () => { if (watching) recover(generation); });
  return { stop, destroy() { stop(); stopped = true; for (const remove of listeners) remove(); } };
}

if (typeof document !== 'undefined') {
  const player = createTvPlayer({ video: document.querySelector('#video'), watch: document.querySelector('#watch'),
    edge: document.querySelector('#live-edge'), status: document.querySelector('#status'),
    indicator: document.querySelector('#indicator'), placeholder: document.querySelector('#placeholder'),
    info: Object.fromEntries(['picture', 'rate', 'codecs', 'buffer', 'delay'].map(key => [key, document.querySelector(`#media-${key}`)])) });
  addEventListener('pagehide', () => player.destroy(), { once: true });
}
