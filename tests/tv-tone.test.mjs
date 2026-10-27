import test from 'node:test';
import assert from 'node:assert/strict';
import { createTestTone } from '../web/tv/tone.mjs';
class Button extends EventTarget {disabled=false;setAttribute(k,v){this[k]=v;}click(){this.dispatchEvent(new Event('click'));}}
const settle=()=>new Promise(r=>setImmediate(r));
function fixture(resume=async()=>{}) {
  const contexts=[]; class Audio {
    destination={};closed=false;started=false;
    constructor(){contexts.push(this);}resume=resume;async close(){this.closed=true;}
    createOscillator(){const owner=this;return {frequency:{value:0},connect(){},disconnect(){},start(){owner.started=true;},stop(){owner.started=false;}};}
    createGain(){return {gain:{value:0},connect(){},disconnect(){}};}
  }
  const button=new Button();return {button,contexts,tone:createTestTone(button,Audio)};
}
test('tone stays silent until clicked and stops for live playback',async()=>{
  const s=fixture();assert.equal(s.contexts.length,0);s.button.click();await settle();assert(s.contexts[0].started);
  s.tone.setEnabled(false);assert(s.contexts[0].closed);assert.equal(s.button['aria-pressed'],'false');
  s.button.click();await settle();assert.equal(s.contexts.length,1);s.tone.destroy();
});
test('a late audio resume cannot start a stopped tone',async()=>{
  let resume;const s=fixture(()=>new Promise(r=>{resume=r;}));s.button.click();s.tone.stop();resume();await settle();
  assert(!s.contexts[0].started);assert(s.contexts[0].closed);s.tone.destroy();
});
