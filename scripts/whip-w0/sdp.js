'use strict';
// Minimal SDP (RFC 8866) offer reader and ICE-lite answer builder for W0.
const CRLF = '\r\n';

function parseOffer(text) {
  const lines = text.split(/\r?\n/).filter(Boolean);
  const session = [], sections = [];
  let cur = null;
  for (const l of lines) {
    if (l.startsWith('m=')) { cur = { m: l, lines: [] }; sections.push(cur); } else (cur ? cur.lines : session).push(l);
  }
  for (const s of sections) {
    const [kind, port, proto, ...pts] = s.m.slice(2).split(' ');
    Object.assign(s, { kind, port: +port, proto, pts });
    const get = (p) => s.lines.filter((x) => x.startsWith(p)).map((x) => x.slice(p.length));
    s.mid = (get('a=mid:')[0] || '').trim();
    s.dir = ['sendonly', 'recvonly', 'sendrecv', 'inactive'].find((d) => s.lines.includes('a=' + d)) || 'sendrecv';
    s.rtpmap = {}; s.fmtp = {}; s.fb = {}; s.extmap = [];
    for (const l of s.lines) {
      let m;
      if ((m = /^a=rtpmap:(\d+) (.+)$/.exec(l))) s.rtpmap[m[1]] = m[2];
      else if ((m = /^a=fmtp:(\d+) (.+)$/.exec(l))) s.fmtp[m[1]] = m[2];
      else if ((m = /^a=rtcp-fb:(\S+) (.+)$/.exec(l))) (s.fb[m[1]] = s.fb[m[1]] || []).push(m[2]);
      else if ((m = /^a=extmap:(\d+)(?:\/\S+)? (\S+)/.exec(l))) s.extmap.push({ id: m[1], uri: m[2] });
    }
  }
  return { session, sections };
}

const H264_PROFILES = new Set(['42', '4d', '64']); // Baseline/Constrained Baseline, Main, High

function h264Info(fmtp) {
  const kv = Object.fromEntries((fmtp || '').split(';').map((p) => p.trim().split('=')).filter((p) => p[0]));
  return { mode: kv['packetization-mode'], profile: (kv['profile-level-id'] || '').toLowerCase() };
}

function buildAnswer(offerText, { ufrag, pwd, fingerprint, candidates, opusFmtp, transportCc = false }) {
  const offer = parseOffer(offerText);
  const out = ['v=0', 'o=- 1 1 IN IP4 127.0.0.1', 's=-', 't=0 0', 'a=ice-lite'];
  const mids = [], body = [];
  let firstAccepted = true;
  for (const s of offer.sections) {
    const accept = (s.kind === 'video' || s.kind === 'audio') && s.dir !== 'recvonly' && s.dir !== 'inactive' &&
      !body.some((b) => b.kind === s.kind);
    if (!accept) { body.push({ kind: s.kind, text: [`m=${s.kind} 0 ${s.proto} ${s.pts[0] || '0'}`, 'c=IN IP4 0.0.0.0', `a=mid:${s.mid}`] }); continue; }
    let pts = [];
    const extra = [];
    if (s.kind === 'video') {
      const h = s.pts.find((pt) => /^H264\//i.test(s.rtpmap[pt] || '') && (() => {
        const i = h264Info(s.fmtp[pt]); return i.mode === '1' && H264_PROFILES.has(i.profile.slice(0, 2));
      })());
      if (!h) { body.push({ kind: s.kind, text: [`m=${s.kind} 0 ${s.proto} ${s.pts[0]}`, 'c=IN IP4 0.0.0.0', `a=mid:${s.mid}`], noH264: true }); continue; }
      pts = [h];
      const rtx = s.pts.find((pt) => /^rtx\//i.test(s.rtpmap[pt] || '') && (s.fmtp[pt] || '').includes(`apt=${h}`));
      if (rtx) pts.push(rtx);
      for (const pt of pts) {
        extra.push(`a=rtpmap:${pt} ${s.rtpmap[pt]}`);
        if (s.fmtp[pt]) extra.push(`a=fmtp:${pt} ${s.fmtp[pt]}`);
      }
      const want = ['nack', 'nack pli', 'ccm fir'].concat(transportCc ? ['transport-cc'] : []);
      for (const w of want) if ((s.fb[h] || []).includes(w)) extra.push(`a=rtcp-fb:${h} ${w}`);
    } else {
      const o = s.pts.find((pt) => /^opus\/48000\/2$/i.test(s.rtpmap[pt] || ''));
      if (!o) { body.push({ kind: s.kind, text: [`m=${s.kind} 0 ${s.proto} ${s.pts[0]}`, 'c=IN IP4 0.0.0.0', `a=mid:${s.mid}`] }); continue; }
      pts = [o];
      extra.push(`a=rtpmap:${o} ${s.rtpmap[o]}`, `a=fmtp:${o} ${opusFmtp}`);
    }
    const t = [`m=${s.kind} 9 ${s.proto} ${pts.join(' ')}`, 'c=IN IP4 0.0.0.0', `a=ice-ufrag:${ufrag}`, `a=ice-pwd:${pwd}`,
      `a=fingerprint:sha-256 ${fingerprint}`, 'a=setup:passive', `a=mid:${s.mid}`];
    const mid = s.extmap.find((e) => e.uri === 'urn:ietf:params:rtp-hdrext:sdes:mid');
    if (mid) t.push(`a=extmap:${mid.id} ${mid.uri}`);
    t.push('a=recvonly', 'a=rtcp-mux', ...extra);
    if (firstAccepted) { candidates.forEach((c) => t.push('a=' + c)); t.push('a=end-of-candidates'); firstAccepted = false; }
    mids.push(s.mid);
    body.push({ kind: s.kind, text: t });
  }
  out.push(`a=group:BUNDLE ${mids.join(' ')}`);
  body.forEach((b) => out.push(...b.text));
  return { sdp: out.join(CRLF) + CRLF, noH264: body.some((b) => b.noH264) };
}

module.exports = { parseOffer, buildAnswer, h264Info };
