'use strict';
// Static server for A0.2: our own, with Range support, so the assets are served the way a CDN would.
// Serves pages/, /assets/ (the asset directory) and /vendor/ (hls.js, fetched into scratch, never the repo).
const http = require('node:http');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const ports = require('./ports.js');

const TYPES = { '.m3u8': 'application/vnd.apple.mpegurl', '.m4s': 'video/iso.segment', '.mp4': 'video/mp4', '.html': 'text/html', '.js': 'text/javascript', '.webm': 'audio/webm' };

function startPlayServer({ assetsDir, vendorDir, pagesDir = path.join(__dirname, 'pages'), outDir, range, host = '127.0.0.1' }) {
  fs.mkdirSync(outDir, { recursive: true });
  const waiters = new Map(), done = new Set(), log = [];
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    if (req.method === 'POST') {
      const chunks = []; req.on('data', (c) => chunks.push(c));
      req.on('end', () => {
        if (url.pathname.startsWith('/result/')) fs.writeFileSync(path.join(outDir, path.basename(url.pathname)), Buffer.concat(chunks));
        if (url.pathname === '/done') { const l = url.searchParams.get('label'); done.add(l); (waiters.get(l) || []).forEach((f) => f()); }
        res.writeHead(204).end();
      });
      return;
    }
    if (url.pathname === '/loadavg') { res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify(os.loadavg())); return; }
    const roots = [['/assets/', assetsDir], ['/vendor/', vendorDir]];
    let file = null;
    for (const [p, dir] of roots) if (url.pathname.startsWith(p)) file = path.join(dir, path.normalize(url.pathname.slice(p.length)).replace(/^(\.\.[/\\])+/, ''));
    if (!file) file = path.join(pagesDir, path.basename(url.pathname === '/' ? 'index.html' : url.pathname));
    let st; try { st = fs.statSync(file); } catch { res.writeHead(404).end(); return; }
    if (!st.isFile()) { res.writeHead(404).end(); return; }
    const headers = { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream', 'Accept-Ranges': 'bytes', 'Cache-Control': 'no-store' };
    const m = /^bytes=(\d*)-(\d*)$/.exec(req.headers.range || '');
    log.push(path.relative(assetsDir || '/', file).startsWith('..') ? url.pathname : url.pathname);
    if (m) {
      const start = m[1] === '' ? st.size - +m[2] : +m[1], end = m[1] !== '' && m[2] !== '' ? Math.min(+m[2], st.size - 1) : st.size - 1;
      res.writeHead(206, { ...headers, 'Content-Range': `bytes ${start}-${end}/${st.size}`, 'Content-Length': end - start + 1 });
      fs.createReadStream(file, { start, end }).pipe(res);
    } else { res.writeHead(200, { ...headers, 'Content-Length': st.size }); fs.createReadStream(file).pipe(res); }
  });
  return ports.listenTcp(server, host, range).then((port) => ({
    port, requests: log,
    waitFor(label, ms) { return new Promise((r) => { if (done.has(label)) return r(true); const t = setTimeout(() => r(false), ms); waiters.set(label, [...(waiters.get(label) || []), () => { clearTimeout(t); r(true); }]); }); },
    close() { server.close(); },
  }));
}
module.exports = { startPlayServer };
