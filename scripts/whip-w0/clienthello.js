'use strict';
// DTLS/TLS ClientHello summariser, written from RFC 6347 (record and handshake
// headers), RFC 5246 and RFC 8446 (ClientHello layout), RFC 5764 (use_srtp),
// RFC 7627, RFC 5746, RFC 8422 and the IANA TLS registries.
// Usage: node clienthello.js <file.hex | file.bin>   (hex text or raw datagram)
const fs = require('node:fs');

const SUITES = {
  0x00ff: 'TLS_EMPTY_RENEGOTIATION_INFO_SCSV', 0x5600: 'TLS_FALLBACK_SCSV',
  0xc02b: 'TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256', 0xc02c: 'TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384',
  0xc02f: 'TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256', 0xc030: 'TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384',
  0xcca8: 'TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256', 0xcca9: 'TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256',
  0xc009: 'TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA', 0xc00a: 'TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA',
  0xc013: 'TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA', 0xc014: 'TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA',
  0xc023: 'TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA256', 0xc024: 'TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA384',
  0xc027: 'TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA256', 0xc028: 'TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA384',
  0x009c: 'TLS_RSA_WITH_AES_128_GCM_SHA256', 0x009d: 'TLS_RSA_WITH_AES_256_GCM_SHA384',
  0x002f: 'TLS_RSA_WITH_AES_128_CBC_SHA', 0x0035: 'TLS_RSA_WITH_AES_256_CBC_SHA',
  0x003c: 'TLS_RSA_WITH_AES_128_CBC_SHA256', 0x003d: 'TLS_RSA_WITH_AES_256_CBC_SHA256',
  0x000a: 'TLS_RSA_WITH_3DES_EDE_CBC_SHA',
  0x1301: 'TLS_AES_128_GCM_SHA256', 0x1302: 'TLS_AES_256_GCM_SHA384', 0x1303: 'TLS_CHACHA20_POLY1305_SHA256',
  0x009e: 'TLS_DHE_RSA_WITH_AES_128_GCM_SHA256', 0x009f: 'TLS_DHE_RSA_WITH_AES_256_GCM_SHA384',
  0xccaa: 'TLS_DHE_RSA_WITH_CHACHA20_POLY1305_SHA256', 0x0033: 'TLS_DHE_RSA_WITH_AES_128_CBC_SHA',
  0x0039: 'TLS_DHE_RSA_WITH_AES_256_CBC_SHA', 0x0067: 'TLS_DHE_RSA_WITH_AES_128_CBC_SHA256',
  0x006b: 'TLS_DHE_RSA_WITH_AES_256_CBC_SHA256',
  0xc0ac: 'TLS_ECDHE_ECDSA_WITH_AES_128_CCM', 0xc0ad: 'TLS_ECDHE_ECDSA_WITH_AES_256_CCM',
};
const EXT = {
  0: 'server_name', 1: 'max_fragment_length', 5: 'status_request', 10: 'supported_groups',
  11: 'ec_point_formats', 13: 'signature_algorithms', 14: 'use_srtp', 15: 'heartbeat',
  16: 'application_layer_protocol_negotiation', 18: 'signed_certificate_timestamp', 21: 'padding',
  22: 'encrypt_then_mac', 23: 'extended_master_secret', 27: 'compress_certificate', 35: 'session_ticket',
  41: 'pre_shared_key', 42: 'early_data', 43: 'supported_versions', 44: 'cookie',
  45: 'psk_key_exchange_modes', 47: 'certificate_authorities', 50: 'signature_algorithms_cert',
  51: 'key_share', 17513: 'application_settings', 65037: 'encrypted_client_hello', 65281: 'renegotiation_info',
};
const GROUPS = {
  0x0017: 'secp256r1', 0x0018: 'secp384r1', 0x0019: 'secp521r1', 0x001d: 'x25519', 0x001e: 'x448',
  0x0100: 'ffdhe2048', 0x0101: 'ffdhe3072', 0x11ec: 'X25519MLKEM768', 0x6399: 'X25519Kyber768Draft00',
  0x11eb: 'SecP256r1MLKEM768', 0x11ed: 'SecP384r1MLKEM1024', 0x0200: 'MLKEM512', 0x0201: 'MLKEM768', 0x0202: 'MLKEM1024',
};
const SIGS = {
  0x0201: 'rsa_pkcs1_sha1', 0x0203: 'ecdsa_sha1', 0x0401: 'rsa_pkcs1_sha256', 0x0403: 'ecdsa_secp256r1_sha256',
  0x0501: 'rsa_pkcs1_sha384', 0x0503: 'ecdsa_secp384r1_sha384', 0x0601: 'rsa_pkcs1_sha512',
  0x0603: 'ecdsa_secp521r1_sha512', 0x0804: 'rsa_pss_rsae_sha256', 0x0805: 'rsa_pss_rsae_sha384',
  0x0806: 'rsa_pss_rsae_sha512', 0x0807: 'ed25519', 0x0808: 'ed448', 0x0809: 'rsa_pss_pss_sha256',
  0x080a: 'rsa_pss_pss_sha384', 0x080b: 'rsa_pss_pss_sha512', 0x0301: 'rsa_pkcs1_sha224', 0x0303: 'ecdsa_sha224',
  0x0402: 'dsa_sha256', 0x0302: 'dsa_sha224', 0x0502: 'dsa_sha384', 0x0602: 'dsa_sha512',
};
const SRTP = {
  0x0001: 'SRTP_AES128_CM_HMAC_SHA1_80', 0x0002: 'SRTP_AES128_CM_HMAC_SHA1_32', 0x0005: 'SRTP_NULL_HMAC_SHA1_80',
  0x0006: 'SRTP_NULL_HMAC_SHA1_32', 0x0007: 'SRTP_AEAD_AES_128_GCM', 0x0008: 'SRTP_AEAD_AES_256_GCM',
};
const VERSIONS = { 0xfeff: 'DTLS 1.0', 0xfefd: 'DTLS 1.2', 0xfefc: 'DTLS 1.3', 0x0301: 'TLS 1.0',
  0x0302: 'TLS 1.1', 0x0303: 'TLS 1.2', 0x0304: 'TLS 1.3' };

const h4 = (n) => '0x' + n.toString(16).padStart(4, '0');
const isGrease = (n) => (n & 0x0f0f) === 0x0a0a && (n >> 8) === (n & 0xff);
const name = (table, n) => (isGrease(n) ? `GREASE(${h4(n)})` : table[n] ? `${table[n]} (${h4(n)})` : `unknown (${h4(n)})`);
const ver = (n) => (VERSIONS[n] ? `${VERSIONS[n]} (${h4(n)})` : `unknown (${h4(n)})`);

function u16list(b) { const o = []; for (let i = 0; i + 1 < b.length; i += 2) o.push(b.readUInt16BE(i)); return o; }

// A capture file holds one or more datagrams as hex, separated by a line "--".
// ClientHello fragments (same message_seq, RFC 6347 section 4.2.3) are reassembled.
function summarise(dgrams) {
  if (Buffer.isBuffer(dgrams)) dgrams = [dgrams];
  const L = [], frags = [];
  dgrams.forEach((dgram, di) => {
    let off = 0, rec = 0;
    L.push(`datagram ${di}: ${dgram.length} bytes`);
    while (off + 13 <= dgram.length) {
      const ctype = dgram[off], rver = dgram.readUInt16BE(off + 1), epoch = dgram.readUInt16BE(off + 3);
      const seq = dgram.readUIntBE(off + 5, 6), len = dgram.readUInt16BE(off + 11);
      const body = dgram.subarray(off + 13, off + 13 + len);
      L.push(`  record ${rec}: content type ${ctype}${ctype === 22 ? ' (handshake)' : ''}, record version ${ver(rver)}, epoch ${epoch}, seq ${seq}, length ${len}`);
      if (ctype === 22 && body.length >= 12) {
        const mt = body[0], mlen = body.readUIntBE(1, 3), mseq = body.readUInt16BE(4);
        const foff = body.readUIntBE(6, 3), flen = body.readUIntBE(9, 3);
        L.push(`    handshake: type ${mt}${mt === 1 ? ' (client_hello)' : ''}, message length ${mlen}, message_seq ${mseq}, fragment offset ${foff}, fragment length ${flen}`);
        if (mt === 1) frags.push({ mlen, mseq, foff, data: body.subarray(12, 12 + flen) });
      }
      off += 13 + len; rec++;
    }
    if (off < dgram.length) L.push(`  trailing bytes: ${dgram.length - off}`);
  });
  const first = frags.find((f) => f.mseq === 0);
  if (first) {
    const msg = Buffer.alloc(first.mlen), have = new Uint8Array(first.mlen);
    frags.filter((f) => f.mseq === 0).forEach((f) => { f.data.copy(msg, f.foff); have.fill(1, f.foff, f.foff + f.data.length); });
    const missing = have.reduce((n, x) => n + (x ? 0 : 1), 0);
    L.push(`client_hello reassembled from ${frags.length} fragment(s): ${first.mlen} bytes${missing ? `, ${missing} bytes MISSING (summary partial)` : ''}`);
    if (!missing) hello(msg, L);
  }
  return L.join('\n');
}

function hello(b, L) {
  let o = 0;
  const cv = b.readUInt16BE(o); o += 2;
  L.push(`  client_version: ${ver(cv)}`);
  o += 32; L.push('  random: 32 bytes (not recorded)');
  const sl = b[o++]; L.push(`  session_id length: ${sl}`); o += sl;
  const cl = b[o++]; L.push(`  cookie length: ${cl}`); o += cl;
  const cs = u16list(b.subarray(o + 2, o + 2 + b.readUInt16BE(o))); o += 2 + b.readUInt16BE(o);
  L.push(`  cipher suites (${cs.length}), in offered order:`);
  cs.forEach((c) => L.push(`    ${name(SUITES, c)}`));
  const nc = b[o++]; L.push(`  compression methods: ${[...b.subarray(o, o + nc)].map((x) => (x === 0 ? 'null' : x)).join(', ')}`); o += nc;
  if (o + 2 > b.length) { L.push('  extensions: none'); return; }
  const el = b.readUInt16BE(o); o += 2;
  const end = o + el, ex = [];
  while (o + 4 <= end) {
    const t = b.readUInt16BE(o), l = b.readUInt16BE(o + 2);
    ex.push({ t, d: b.subarray(o + 4, o + 4 + l) }); o += 4 + l;
  }
  L.push(`  extensions (${ex.length}), in offered order:`);
  ex.forEach(({ t, d }) => L.push(`    ${name(EXT, t)}, ${d.length} bytes`));
  L.push('  extension detail:');
  for (const { t, d } of ex) {
    if (t === 43) {
      const list = u16list(d.subarray(1, 1 + d[0]));
      L.push(`    supported_versions: ${list.map(ver).join(', ')}`);
    } else if (t === 10) {
      L.push(`    supported_groups: ${u16list(d.subarray(2)).map((g) => name(GROUPS, g)).join(', ')}`);
    } else if (t === 11) {
      L.push(`    ec_point_formats: ${[...d.subarray(1)].map((x) => (x === 0 ? 'uncompressed' : x)).join(', ')}`);
    } else if (t === 13 || t === 50) {
      L.push(`    ${EXT[t]}: ${u16list(d.subarray(2)).map((s) => name(SIGS, s)).join(', ')}`);
    } else if (t === 14) {
      const n = d.readUInt16BE(0);
      L.push(`    use_srtp profiles: ${u16list(d.subarray(2, 2 + n)).map((p) => name(SRTP, p)).join(', ')}; MKI length ${d[2 + n]}`);
    } else if (t === 23) {
      L.push(`    extended_master_secret: present (empty, ${d.length} bytes)`);
    } else if (t === 65281) {
      L.push(`    renegotiation_info: present, renegotiated_connection length ${d[0]}`);
    } else if (t === 16) {
      L.push(`    ALPN: ${d.length > 2 ? d.subarray(2).toString('latin1').replace(/[^\x20-\x7e]/g, ' ').trim() : 'empty'}`);
    } else if (t === 51) {
      L.push(`    key_share: ${d.length} bytes (TLS 1.3 key shares offered)`);
    } else if (t === 35) {
      L.push(`    session_ticket: ${d.length} bytes`);
    } else if (t === 22) {
      L.push('    encrypt_then_mac: present');
    }
  }
  const has = (t) => ex.some((e) => e.t === t);
  L.push('  presence: ' + [
    ['extended_master_secret', 23], ['renegotiation_info', 65281], ['use_srtp', 14], ['supported_versions', 43],
    ['key_share', 51], ['session_ticket', 35], ['encrypt_then_mac', 22], ['server_name', 0], ['ALPN', 16],
  ].map(([n, t]) => `${n}=${has(t) ? 'yes' : 'no'}`).join(' '));
}

function load(file) {
  const raw = fs.readFileSync(file);
  const txt = raw.toString('latin1').trim();
  if (!/^[0-9a-fA-F\s-]+$/.test(txt)) return [raw];
  return txt.split(/^--$/m).map((p) => Buffer.from(p.replace(/\s+/g, ''), 'hex')).filter((b) => b.length);
}

if (require.main === module) {
  if (!process.argv[2]) { console.error('usage: node clienthello.js <file.hex|file.bin>'); process.exit(2); }
  console.log(summarise(load(process.argv[2])));
}
module.exports = { summarise, load };
