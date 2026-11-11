'use strict';
// STUN (RFC 8489) message codec for the W0 fake endpoint. Written from the RFC
// and checked against the RFC 5769 vectors (see stun-rfc5769.test.js).
const crypto = require('node:crypto');
const zlib = require('node:zlib');

const MAGIC = 0x2112a442;
const ATTR = {
  USERNAME: 0x0006, MESSAGE_INTEGRITY: 0x0008, ERROR_CODE: 0x0009,
  XOR_MAPPED_ADDRESS: 0x0020, PRIORITY: 0x0024, USE_CANDIDATE: 0x0025,
  SOFTWARE: 0x8022, FINGERPRINT: 0x8028, ICE_CONTROLLED: 0x8029, ICE_CONTROLLING: 0x802a,
};
const ATTR_NAMES = Object.fromEntries(Object.entries(ATTR).map(([k, v]) => [v, k]));

function looksLikeStun(buf) {
  // RFC 7983: first byte 0..3; length multiple of 4; magic cookie present.
  return buf.length >= 20 && buf[0] < 4 && (buf.length - 20) === buf.readUInt16BE(2) &&
    buf.readUInt32BE(4) === MAGIC;
}

function parse(buf) {
  if (!looksLikeStun(buf)) throw new Error('not a STUN message');
  const msg = { type: buf.readUInt16BE(0), tid: buf.subarray(8, 20), attrs: [] };
  let off = 20;
  while (off + 4 <= buf.length) {
    const type = buf.readUInt16BE(off), len = buf.readUInt16BE(off + 2);
    if (off + 4 + len > buf.length) throw new Error('attribute overruns message');
    msg.attrs.push({ type, offset: off, value: buf.subarray(off + 4, off + 4 + len) });
    off += 4 + ((len + 3) & ~3);
  }
  return msg;
}

const find = (msg, type) => msg.attrs.find((a) => a.type === type);

function hmacOver(buf, miOffset, key) {
  // Length field is rewritten to cover everything up to and including the
  // MESSAGE-INTEGRITY attribute (24 bytes), then the HMAC covers the header and
  // the attributes before it (RFC 8489 section 14.5).
  const head = Buffer.from(buf.subarray(0, miOffset));
  head.writeUInt16BE(miOffset - 20 + 24, 2);
  return crypto.createHmac('sha1', key).update(head).digest();
}

function fingerprintOver(buf, fpOffset) {
  const head = Buffer.from(buf.subarray(0, fpOffset));
  head.writeUInt16BE(fpOffset - 20 + 8, 2);
  return (zlib.crc32(head) ^ 0x5354554e) >>> 0;
}

function checkIntegrity(buf, msg, key) {
  const a = find(msg, ATTR.MESSAGE_INTEGRITY);
  if (!a || a.value.length !== 20) return false;
  return crypto.timingSafeEqual(a.value, hmacOver(buf, a.offset, key));
}

function checkFingerprint(buf, msg) {
  const a = find(msg, ATTR.FINGERPRINT);
  if (!a || a.value.length !== 4) return false;
  return a.value.readUInt32BE(0) === fingerprintOver(buf, a.offset);
}

function xorAddress(ip, port, tid) {
  const v6 = ip.includes(':');
  const out = Buffer.alloc(v6 ? 20 : 8);
  out[1] = v6 ? 2 : 1;
  out.writeUInt16BE(port ^ (MAGIC >>> 16), 2);
  const mask = Buffer.alloc(16);
  mask.writeUInt32BE(MAGIC, 0);
  tid.copy(mask, 4);
  const bytes = v6 ? ipv6Bytes(ip) : Buffer.from(ip.split('.').map(Number));
  for (let i = 0; i < bytes.length; i++) out[4 + i] = bytes[i] ^ mask[i];
  return out;
}

function ipv6Bytes(ip) {
  const [l, r = ''] = ip.split('::');
  const lp = l ? l.split(':') : [], rp = r ? r.split(':') : [];
  const parts = ip.includes('::') ? [...lp, ...Array(8 - lp.length - rp.length).fill('0'), ...rp] : lp;
  const b = Buffer.alloc(16);
  parts.forEach((p, i) => b.writeUInt16BE(parseInt(p, 16), i * 2));
  return b;
}

function pad(v) { return Buffer.concat([v, Buffer.alloc((4 - (v.length % 4)) % 4)]); }
function attr(type, value) {
  const h = Buffer.alloc(4);
  h.writeUInt16BE(type, 0); h.writeUInt16BE(value.length, 2);
  return Buffer.concat([h, pad(value)]);
}

// Build a message; attrs is [type, value][]; adds MESSAGE-INTEGRITY if key is
// given, then FINGERPRINT if fingerprint is true.
function build(type, tid, attrs, { key, fingerprint = true, padByte = 0 } = {}) {
  const mk = (list) => {
    const body = Buffer.concat(list);
    const h = Buffer.alloc(20);
    h.writeUInt16BE(type, 0); h.writeUInt16BE(body.length, 2); h.writeUInt32BE(MAGIC, 4);
    tid.copy(h, 8);
    return Buffer.concat([h, body]);
  };
  const parts = attrs.map(([t, v]) => {
    const a = attr(t, v);
    if (padByte) a.fill(padByte, 4 + v.length); // RFC 5769 pads with spaces
    return a;
  });
  if (key) {
    const m = mk(parts);
    parts.push(attr(ATTR.MESSAGE_INTEGRITY, hmacOver(m, m.length, key)));
  }
  if (fingerprint) {
    const m = mk(parts);
    const f = Buffer.alloc(4);
    f.writeUInt32BE(fingerprintOver(m, m.length));
    parts.push(attr(ATTR.FINGERPRINT, f));
  }
  return mk(parts);
}

module.exports = { MAGIC, ATTR, ATTR_NAMES, looksLikeStun, parse, find, checkIntegrity, checkFingerprint,
  hmacOver, fingerprintOver, xorAddress, build, attr };
