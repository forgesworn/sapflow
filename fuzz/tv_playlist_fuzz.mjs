// Finite deterministic mutations, not coverage-guided fuzzing. No network I/O.
import assert from 'node:assert/strict';
import {parseMaster,parseMedia} from '../web/tv/playlist.mjs';
const name='012345abcdef',seeds=[`#EXTM3U\n#EXT-X-MAP:URI="${name}_init.mp4"\n#EXTINF:1,\n${name}_seg1.mp4\n`,
 '#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1100000,CODECS="avc1.640029,mp4a.40.2"\nstream.m3u8\n',
 '#EXTM3U\n#EXT-X-KEY:URI="https://evil.example/key"\n','#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:9007199254740991\n'];
let state=20261027;const random=()=>{state^=state<<13;state^=state>>>17;state^=state<<5;return state>>>0;};
const count=Number(process.argv[2]??20000);
for(let i=0;i<count;i++) {
 let text=seeds[random()%seeds.length];
 for(let j=0;j<1+random()%5;j++){const pos=random()%(text.length+1);text=text.slice(0,pos)+String.fromCharCode(random()%256)+text.slice(pos+(random()%3));}
 for(const parse of [parseMaster,parseMedia]) {
  let value;try{value=parse(text);}catch{continue;}
  if(value.segments)for(const segment of value.segments){assert(Number.isSafeInteger(segment.sn));assert(Number.isSafeInteger(segment.cc));assert(!/[:/\\]/.test(segment.name));assert(segment.duration>0&&segment.duration<=3600);}
  else {assert.equal(value.manifest,'stream.m3u8');assert(/^avc1\.[a-f0-9]{6},mp4a\.40\.2$/i.test(value.codec));}
 }
}
console.log(`tv_playlist mutation executions=${count}; coverage_guided=false`);
