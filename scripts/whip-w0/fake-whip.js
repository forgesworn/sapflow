'use strict';
// Fake WHIP endpoint for W0 evidence. ICE-lite, answers STUN, saves the offer
// and the first DTLS datagram, never completes DTLS. Also serves the capture
// pages and collects their JSON results. No dependencies.
const http = require('node:http');
const dgram = require('node:dgram');
const net = require('node:net');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const stun = require('./stun.js');
const { buildAnswer } = require('./sdp.js');
const { startRelay } = require('./l4-relay.js');
const ports = require('./ports.js');

function makeCert(dir) {
  const key = path.join(dir, 'key.pem'), crt = path.join(dir, 'cert.pem');
  execFileSync('openssl', ['req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes',
    '-keyout', key, '-out', crt, '-subj', '/CN=w0', '-days', '2'], { stdio: 'ignore' });
  return new crypto.X509Certificate(fs.readFileSync(crt)).fingerprint256; // SHA-256, colon-separated
}

// Start the endpoint. mode: 'udp' or 'tcp'. outDir receives raw captures.
async function start({ mode = 'udp', host, advertise, httpPort = 0, udpPort = 0, relayPort = 443, outDir, label = mode,
  pagesDir = path.join(__dirname, 'pages'), transportCc = false, range = null, opusFmtp = 'stereo=1;maxaveragebitrate=128000;usedtx=0' }) {
  fs.mkdirSync(outDir, { recursive: true });
  const fingerprint = makeCert(outDir);
  const ufrag = crypto.randomBytes(4).toString('hex'), pwd = crypto.randomBytes(16).toString('hex');
  const key = Buffer.from(pwd);
  const ev = { label, mode, stun: { requests: 0, valid: 0, badIntegrity: 0, noFingerprint: 0, attrs: {}, useCandidate: 0, controlling: 0, controlled: 0, tuples: 0 },
    dtls: { datagrams: 0, firstTransport: null, firstLength: null }, tcpConnections: 0, whipRequests: [], other: 0, loadStart: os.loadavg(), results: [], done: false };
  const seenTuples = new Set(), dtlsSeen = new Set(), dtlsKept = [];
  let finish = () => {};
  const donePromise = new Promise((r) => { finish = r; });

  const handle = (buf, remote, transport, reply) => {
    if (stun.looksLikeStun(buf)) {
      ev.stun.requests++;
      let m;
      try { m = stun.parse(buf); } catch { return; }
      if (m.type !== 0x0001) return;
      const user = stun.find(m, stun.ATTR.USERNAME);
      if (!user || !user.value.toString().startsWith(ufrag + ':')) return;
      if (!stun.checkIntegrity(buf, m, key)) { ev.stun.badIntegrity++; return; }
      if (!stun.checkFingerprint(buf, m)) ev.stun.noFingerprint++;
      ev.stun.valid++;
      const t = transport + ' ' + remote.address + ':' + remote.port;
      if (!seenTuples.has(t)) { seenTuples.add(t); ev.stun.tuples++; }
      m.attrs.forEach((a) => { const n = stun.ATTR_NAMES[a.type] || '0x' + a.type.toString(16); ev.stun.attrs[n] = (ev.stun.attrs[n] || 0) + 1; });
      if (stun.find(m, stun.ATTR.USE_CANDIDATE)) ev.stun.useCandidate++;
      if (stun.find(m, stun.ATTR.ICE_CONTROLLING)) ev.stun.controlling++;
      if (stun.find(m, stun.ATTR.ICE_CONTROLLED)) {
        ev.stun.controlled++;
        reply(stun.build(0x0111, m.tid, [[stun.ATTR.ERROR_CODE, Buffer.concat([Buffer.from([0, 0, 4, 87]), Buffer.from('Role Conflict')])]], { key }));
        return;
      }
      reply(stun.build(0x0101, m.tid, [[stun.ATTR.XOR_MAPPED_ADDRESS, stun.xorAddress(remote.address, remote.port, m.tid)]], { key }));
    } else if (buf.length > 0 && buf[0] >= 20 && buf[0] <= 63) {
      ev.dtls.datagrams++;
      // Keep the first datagrams that carry new ClientHello fragments (the
      // hello can span datagrams, RFC 6347 section 4.2.3). Retransmits are counted only.
      const isHello = buf.length >= 25 && buf[0] === 22 && buf[13] === 1 && buf.readUInt16BE(17) === 0;
      if (ev.dtls.firstLength === null) { ev.dtls.firstTransport = transport; ev.dtls.firstLength = buf.length; }
      if (isHello) {
        const off = buf.readUIntBE(19, 3);
        if (!dtlsSeen.has(off) && dtlsKept.length < 4) {
          dtlsSeen.add(off); dtlsKept.push(buf);
          fs.writeFileSync(path.join(outDir, `${label}-clienthello.hex`),
            dtlsKept.map((d) => d.toString('hex').replace(/(.{64})/g, '$1\n')).join('\n--\n') + '\n');
          ev.dtls.helloFragments = dtlsKept.map((d) => d.length);
        }
      }
    } else ev.other++;
  };

  // UDP
  const udp = dgram.createSocket('udp4');
  udp.on('message', (b, r) => handle(b, r, 'udp', (out) => udp.send(out, r.port, r.address)));
  const udpP = await ports.bindUdp(udp, '0.0.0.0', range, udpPort);

  // TCP with RFC 4571 framing (2-byte length, then the packet), behind the relay.
  let tcpSrv = null, relay = null, tcpPort = 0;
  if (mode === 'tcp') {
    tcpSrv = net.createServer((sock) => {
      let acc = Buffer.alloc(0);
      const remote = { address: sock.remoteAddress, port: sock.remotePort };
      sock.on('data', (d) => {
        acc = Buffer.concat([acc, d]);
        while (acc.length >= 2 && acc.length >= 2 + acc.readUInt16BE(0)) {
          const n = acc.readUInt16BE(0), pkt = acc.subarray(2, 2 + n);
          acc = acc.subarray(2 + n);
          handle(pkt, remote, 'tcp', (out) => { const f = Buffer.alloc(2); f.writeUInt16BE(out.length); sock.write(Buffer.concat([f, out])); });
        }
      });
      sock.on('error', () => {});
    });
    const backend = await ports.listenTcp(tcpSrv, '127.0.0.1', range);
    try {
      relay = await startRelay({ listenHost: relayPort < 1024 ? '0.0.0.0' : host, listenPort: relayPort, targetHost: '127.0.0.1', targetPort: backend, onConnection: () => { ev.tcpConnections++; } });
    } catch (e) {
      if (relayPort === 443) { relay = await startRelay({ listenHost: host, listenPort: 0, targetHost: '127.0.0.1', targetPort: backend, onConnection: () => { ev.tcpConnections++; } }); ev.relayFallback = 'port 443 unavailable: ' + e.code; } else throw e;
    }
    tcpPort = relay.port;
  }
  ev.relayPort = tcpPort;

  const ad = advertise || host; // --advertise: an address that is not ours, to test whether Chrome even tries
  const candidates = mode === 'udp'
    ? [`candidate:1 1 udp 2130706431 ${ad} ${udpP} typ host`]
    : [`candidate:1 1 tcp 1518280447 ${ad} ${tcpPort} typ host tcptype passive`];

  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const body = Buffer.concat(chunks);
      if (req.method === 'POST' && url.pathname === '/whip') {
        // Record that a bearer token arrived (scheme only, never the value) and which headers a client sent.
        ev.whipRequests.push({ authScheme: (req.headers.authorization || '').split(' ')[0] || null, contentType: req.headers['content-type'] || null, userAgent: req.headers['user-agent'] || null, headerNames: Object.keys(req.headers).sort() });
        if (!(req.headers['content-type'] || '').startsWith('application/sdp')) { res.writeHead(415).end(); return; }
        const lbl = url.searchParams.get('label') || label;
        fs.writeFileSync(path.join(outDir, `${lbl}-offer.sdp`), body);
        const a = buildAnswer(body.toString(), { ufrag, pwd, fingerprint, candidates, opusFmtp, transportCc });
        fs.writeFileSync(path.join(outDir, `${lbl}-answer.sdp`), a.sdp);
        res.writeHead(201, { 'Content-Type': 'application/sdp', Location: '/whip/1', 'Access-Control-Expose-Headers': 'Location' });
        res.end(a.sdp);
      } else if (req.method === 'DELETE') { ev.whipDelete = (ev.whipDelete || 0) + 1; res.writeHead(200).end();
      } else if (req.method === 'PATCH') { ev.whipPatch = (ev.whipPatch || 0) + 1; ev.whipPatchTypes = [...new Set([...(ev.whipPatchTypes || []), req.headers['content-type'] || ''])]; res.writeHead(204).end();
      } else if (req.method === 'POST' && url.pathname.startsWith('/result/')) {
        const n = path.basename(url.pathname);
        fs.writeFileSync(path.join(outDir, n), body);
        ev.results.push(n);
        res.writeHead(204).end();
      } else if (req.method === 'POST' && url.pathname === '/done') { res.writeHead(204).end(); ev.done = true; finish(); 
      } else if (url.pathname === '/loadavg') { res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify(os.loadavg()));
      } else if (req.method === 'GET') {
        const f = path.join(pagesDir, path.basename(url.pathname === '/' ? 'index.html' : url.pathname));
        if (fs.existsSync(f)) { res.writeHead(200, { 'Content-Type': f.endsWith('.js') ? 'text/javascript' : 'text/html' }).end(fs.readFileSync(f)); } else res.writeHead(404).end();
      } else res.writeHead(404).end();
    });
  });
  const httpBound = await ports.listenTcp(server, '127.0.0.1', range, httpPort);

  return {
    httpPort: httpBound, udpPort: udpP, tcpPort, ev, done: donePromise,
    close() {
      ev.loadEnd = os.loadavg();
      fs.writeFileSync(path.join(outDir, `${label}-events.json`), JSON.stringify(ev, null, 2));
      udp.close(); server.close(); if (tcpSrv) tcpSrv.close(); if (relay) relay.close();
    },
  };
}

module.exports = { start };
