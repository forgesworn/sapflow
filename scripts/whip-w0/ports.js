'use strict';
// Bind inside a fixed port range, so a run stays out of ports that other runs
// on the same host use. range is [low, high] inclusive, or null for any port.
const net = require('node:net');
const dgram = require('node:dgram');

function parseRange(text) {
  if (!text) return null;
  const m = /^(\d+)-(\d+)$/.exec(text);
  if (!m || +m[1] > +m[2] || +m[2] > 65535) throw new Error('bad port range: ' + text);
  return [+m[1], +m[2]];
}

// attempt(port) resolves with its result or rejects with an error that has a code.
async function inRange(range, attempt) {
  if (!range) return attempt(0);
  const [lo, hi] = range, n = hi - lo + 1, start = Math.floor(Math.random() * n);
  for (let i = 0; i < n; i++) {
    try { return await attempt(lo + ((start + i) % n)); } catch (e) { if (e.code !== 'EADDRINUSE') throw e; }
  }
  throw new Error(`no free port in ${lo}-${hi}`);
}

function listenTcp(server, host, range, fixed = 0) {
  return inRange(fixed ? [fixed, fixed] : range, (p) => new Promise((res, rej) => {
    const onErr = (e) => rej(e);
    server.once('error', onErr);
    server.listen(p, host, () => { server.removeListener('error', onErr); res(server.address().port); });
  }));
}

function bindUdp(sock, host, range, fixed = 0) {
  return inRange(fixed ? [fixed, fixed] : range, (p) => new Promise((res, rej) => {
    const onErr = (e) => rej(e);
    sock.once('error', onErr);
    sock.bind(p, host, () => { sock.removeListener('error', onErr); res(sock.address().port); });
  }));
}

module.exports = { parseRange, inRange, listenTcp, bindUdp, net, dgram };
