'use strict';
// Summarise a captured WHIP offer (A0.1, A0.5): codecs, H.264 profile-level-id
// and packetization-mode, feedback, header extensions, DTLS setup, candidate kinds.
// Usage: node offer-summary.js <offer.sdp>
const fs = require('node:fs');
const { parseOffer, h264Info } = require('./sdp.js');

function summarise(text) {
  const o = parseOffer(text), L = [];
  const g = o.session.find((l) => l.startsWith('a=group:'));
  L.push(`session: ${g || 'no BUNDLE group'}; ice-lite=${o.session.includes('a=ice-lite') ? 'yes' : 'no'}`);
  for (const s of o.sections) {
    L.push(`${s.kind} section: mid=${s.mid} direction=${s.dir} proto=${s.proto}`);
    // setup, fingerprint, ice-options and credentials may sit at session level (Firefox and OBS do): read both.
    const has = (p) => s.lines.concat(o.session).filter((l) => l.startsWith(p)).map((l) => l.slice(p.length));
    L.push(`  setup=${has('a=setup:').join(',') || 'absent'} rtcp-mux=${s.lines.includes('a=rtcp-mux') ? 'yes' : 'no'} fingerprint=${has('a=fingerprint:').map((f) => f.split(' ')[0]).join(',') || 'absent'} ice-options=${has('a=ice-options:').join(',') || 'absent'}`);
    const cands = s.lines.filter((l) => l.startsWith('a=candidate:')).map((l) => {
      const p = l.split(' ');
      return `${p[2]} ${p[7]}${p[8] === 'tcptype' ? ' ' + p[9] : ''}`;
    });
    const count = {}; cands.forEach((c) => { count[c] = (count[c] || 0) + 1; });
    L.push(`  candidates: ${Object.entries(count).map(([k, v]) => `${v} x ${k}`).join('; ') || 'none'}`);
    L.push(`  ssrc lines: ${s.lines.filter((l) => l.startsWith('a=ssrc:')).length}, ssrc-group: ${has('a=ssrc-group:').join(' | ') || 'none'}`);
    L.push(`  header extensions: ${s.extmap.map((e) => e.uri.replace(/^urn:ietf:params:rtp-hdrext:/, '').replace(/^http:\/\/www\.(webrtc\.org\/experiments|ietf\.org\/id)\//, '')).join(', ') || 'none'}`);
    L.push('  payloads in offer order:');
    for (const pt of s.pts) {
      let d = `    ${pt} ${s.rtpmap[pt] || '(no rtpmap)'}`;
      if (/^H264\//i.test(s.rtpmap[pt] || '')) { const i = h264Info(s.fmtp[pt]); d += `  profile-level-id=${i.profile || '-'} packetization-mode=${i.mode || '(absent, defaults to 0)'}`; }
      else if (s.fmtp[pt]) d += `  fmtp: ${s.fmtp[pt]}`;
      if (s.fb[pt]) d += `  rtcp-fb: ${s.fb[pt].join(', ')}`;
      L.push(d);
    }
  }
  const v = o.sections.filter((s) => s.kind === 'video');
  const names = [...new Set(v.flatMap((s) => s.pts.map((p) => (s.rtpmap[p] || '').split('/')[0]).filter((n) => n && !/^(rtx|red|ulpfec|flexfec-03)$/i.test(n))))];
  L.push(`video codecs offered (excluding rtx/red/fec): ${names.join(', ') || 'none'}`);
  const h = v.flatMap((s) => s.pts.filter((p) => /^H264\//i.test(s.rtpmap[p] || '')).map((p) => h264Info(s.fmtp[p])));
  L.push(`H.264: ${h.length ? h.map((i) => `${i.profile}/pm${i.mode ?? '?'}`).join(', ') : 'NOT OFFERED'}`);
  return L.join('\n');
}

if (require.main === module) console.log(summarise(fs.readFileSync(process.argv[2], 'utf8')));
module.exports = { summarise };
