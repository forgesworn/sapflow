'use strict';
// RFC 5769 test vectors against stun.js. Run: node scripts/whip-w0/stun-rfc5769.test.js
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const stun = require('./stun.js');

const hex = (s) => Buffer.from(s.replace(/\s+/g, ''), 'hex');
const PASS = 'VOkJxbRl1RmTxUk/WvJxBt';

const request = hex(`
 000100582112a442b7e7a701bc34d686fa87dfae802200105354554e20746573742063
 6c69656e74002400046e0001ff80290008932ff9b151263b3600060009 6576746a3a6836765920202000
 080014 9aeaa70cbfd8cb56781ef2b5b2d3f249c1b571a2 80280004 e57a3bcf`);
const resp4 = hex(`
 0101003c2112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220
 002000080001a147e112a643
 000800142b91f599fd9e90c38c7489f92af9ba53f06be7d7 80280004 c07d4c96`);
const resp6 = hex(`
 010100482112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220
 002000140002a1470113a9faa5d3f179bc25f4b5bed2b9d9
 00080014a382954e4be67bf11784c97c8292c275bfe3ed41 80280004 c8fb0b4c`);
const longTerm = hex(`
 0001006021 12a44278ad3433c6ad72c029da412e
 00060012e3839ee38388e383aae38383e382afe382b90000
 0015001c662f2f3439396b39353464364f4c33346f4c394653547679363473 41
 0014000b6578616d706c652e6f72670 0
 00080014f67024656dd64a3e02b8e0712e85c9a28ca89666`);

let n = 0;
const ok = (name, fn) => { fn(); n++; console.log('ok  ' + name); };

ok('request: parses, fingerprint and integrity verify (short-term key)', () => {
  const m = stun.parse(request);
  assert.equal(m.type, 0x0001);
  assert.ok(stun.checkFingerprint(request, m));
  assert.ok(stun.checkIntegrity(request, m, Buffer.from(PASS)));
  assert.ok(!stun.checkIntegrity(request, m, Buffer.from('wrong')));
});

ok('request: a flipped bit breaks integrity and fingerprint', () => {
  const bad = Buffer.from(request); bad[30] ^= 1;
  const m = stun.parse(bad);
  assert.ok(!stun.checkFingerprint(bad, m));
  assert.ok(!stun.checkIntegrity(bad, m, Buffer.from(PASS)));
});

const tid = hex('b7e7a701bc34d686fa87dfae');
ok('IPv4 response: verifies, and rebuilding it reproduces the bytes', () => {
  const m = stun.parse(resp4);
  assert.ok(stun.checkFingerprint(resp4, m) && stun.checkIntegrity(resp4, m, Buffer.from(PASS)));
  const rebuilt = stun.build(0x0101, tid, [
    [stun.ATTR.SOFTWARE, Buffer.from('test vector')],
    [stun.ATTR.XOR_MAPPED_ADDRESS, stun.xorAddress('192.0.2.1', 32853, tid)],
  ], { key: Buffer.from(PASS), padByte: 0x20 });
  assert.equal(rebuilt.toString('hex'), resp4.toString('hex'));
});

ok('IPv6 response: verifies, and rebuilding it reproduces the bytes', () => {
  const m = stun.parse(resp6);
  assert.ok(stun.checkFingerprint(resp6, m) && stun.checkIntegrity(resp6, m, Buffer.from(PASS)));
  const rebuilt = stun.build(0x0101, tid, [
    [stun.ATTR.SOFTWARE, Buffer.from('test vector')],
    [stun.ATTR.XOR_MAPPED_ADDRESS, stun.xorAddress('2001:db8:1234:5678:11:2233:4455:6677', 32853, tid)],
  ], { key: Buffer.from(PASS), padByte: 0x20 });
  assert.equal(rebuilt.toString('hex'), resp6.toString('hex'));
});

ok('long-term request: integrity with MD5(user:realm:SASLprepped password) key', () => {
  const m = stun.parse(longTerm);
  const user = Buffer.from(stun.find(m, stun.ATTR.USERNAME).value);
  const key = crypto.createHash('md5').update(Buffer.concat([user, Buffer.from(':example.org:TheMatrIX')])).digest();
  assert.ok(stun.checkIntegrity(longTerm, m, key));
});

ok('xor-mapped-address round trip of the request vector fields', () => {
  assert.equal(stun.xorAddress('192.0.2.1', 32853, tid).toString('hex'), '0001a147e112a643');
});

console.log(`${n} checks passed`);
