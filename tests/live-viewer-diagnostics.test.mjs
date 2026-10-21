import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('./live-viewer.html', import.meta.url), 'utf8');
const context = vm.createContext({ArrayBuffer, Uint8Array, Date, btoa});
vm.runInContext(source.match(/<script id="diagnostics">([\s\S]*?)<\/script>/)[1], context);
const {Evidence, ranges, bytes, loaderClass} = context.SapflowViewerDiagnostics;
const plain = value => JSON.parse(JSON.stringify(value));
assert.deepEqual(plain(ranges({length:2, start:i => [0, 4][i], end:i => [2, 6][i]})), [[0, 2], [4, 6]]);
const evidence = new Evidence(16, 3);
const storage = new Uint8Array([9, 1, 2, 3, 9]);
evidence.loaded({payload:storage.subarray(1, 4), frag:{sn:1, url:'/part1.mp4'}});
storage[1] = 8;
assert.deepEqual([...evidence.media[0].bytes], [1, 2, 3]);
evidence.loaded({payload:new Uint8Array([0, 1]), frag:{sn:'initSegment', url:'/init.mp4'}});
evidence.playlist({networkDetails:{status:200, responseText:'#EXTM3U', getResponseHeader:() => 'application/vnd.apple.mpegurl'}});
const circular = {sn:2, elementaryStreams:{video:{startPTS:1, endPTS:2}}};
circular.loop = circular;
const failure = evidence.error({details:'fragParsingError', error:new Error('no media'), frag:circular,
  networkDetails:{status:200, getResponseHeader:() => 'video/mp4'}}, {currentTime:1, buffered:{length:0}});
assert.equal(failure.message, 'no media');
assert.equal(failure.response.contentType, 'video/mp4');
assert.equal(evidence.incident.playlists[0].text, '#EXTM3U');
for (let sn = 3; sn < 15; sn++) evidence.loaded({payload:new Uint8Array(4).fill(sn), frag:{sn}});
assert.ok(evidence.media.length <= 3 && evidence.used <= 16);
assert.ok(evidence.incidentUsed <= 16);
const beforeOversize = evidence.used;
evidence.loaded({payload:new Uint8Array(17), frag:{sn:999}});
assert.equal(evidence.used, beforeOversize);
assert.equal(evidence.incident.media[0].frag.sn, 1);
const packet = plain(evidence.packet({hlsVersion:'fixture'}));
assert.equal(packet.incident.media[0].base64, 'AQID');
assert.equal(packet.incident.init[0].base64, 'AAE=');
assert.equal(packet.incident.error.frag.elementaryStreams.video.endPTS, 2);
assert.doesNotThrow(() => JSON.stringify(packet));
for (let i = 0; i < 150; i++) evidence.error({details:'fixture'}, null);
assert.equal(evidence.failures.length, 128);
const transferred = new Uint8Array([7, 8]).buffer;
const loaderEvidence = new Evidence();
class Loader {
  load(context, config, callbacks) { callbacks.onSuccess({data:transferred}, {}, context, {}); }
}
const RecordingLoader = loaderClass(Loader, loaderEvidence);
new RecordingLoader().load({frag:{sn:12}}, {}, {onSuccess:() => {
  structuredClone(transferred, {transfer:[transferred]});
}});
assert.deepEqual([...loaderEvidence.media[0].bytes], [7, 8]);
assert.equal(bytes(transferred), null);
assert.equal(loaderEvidence.captureErrors, 0);
console.log('Viewer diagnostics: buffer holes, byte ownership, bounded retention, error context and export passed');
