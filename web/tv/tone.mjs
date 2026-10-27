/** Optional quiet 1 kHz tone. Never starts without a button click. */
export function createTestTone(button, AudioContextImpl=globalThis.AudioContext ?? globalThis.webkitAudioContext) {
  let context=null, oscillator=null, gain=null, wanted=false, generation=0;
  function stop() {
    wanted=false; generation++;
    oscillator?.stop(); oscillator?.disconnect(); gain?.disconnect();
    oscillator=gain=null;
    const previous=context; context=null;
    if (previous) void previous.close().catch(()=>{});
    button.textContent='Play quiet test tone'; button.setAttribute('aria-pressed','false');
  }
  async function toggle() {
    if (wanted) { stop(); return; }
    if (button.disabled || !AudioContextImpl) return;
    wanted=true; const token=++generation;
    const own=new AudioContextImpl(); context=own;
    try {
      await own.resume();
      if (token!==generation || !wanted) return;
      oscillator=own.createOscillator(); gain=own.createGain();
      oscillator.frequency.value=1000; gain.gain.value=0.01;
      oscillator.connect(gain); gain.connect(own.destination); oscillator.start();
      button.textContent='Stop test tone'; button.setAttribute('aria-pressed','true');
    } catch { if (token===generation) stop(); }
  }
  button.addEventListener('click',toggle);
  return {stop, setEnabled(enabled) { button.disabled=!enabled || !AudioContextImpl; if(!enabled) stop(); },
    destroy() {stop();button.removeEventListener('click',toggle);} };
}
