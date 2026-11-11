'use strict';
// A small layer-4 byte relay: accepts TCP on listen, pipes bytes both ways to
// target. No inspection, so RFC 4571 framing passes through untouched. It stands
// in for the front proxy that the design puts on TCP 443.
const net = require('node:net');

function startRelay({ listenHost, listenPort, targetHost, targetPort, onConnection }) {
  const sockets = new Set();
  const server = net.createServer((client) => {
    const upstream = net.connect(targetPort, targetHost);
    sockets.add(client); sockets.add(upstream);
    if (onConnection) onConnection();
    client.pipe(upstream); upstream.pipe(client);
    const end = () => { client.destroy(); upstream.destroy(); sockets.delete(client); sockets.delete(upstream); };
    client.on('error', end); upstream.on('error', end);
    client.on('close', end); upstream.on('close', end);
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(listenPort, listenHost, () => resolve({
      port: server.address().port,
      close: () => { sockets.forEach((s) => s.destroy()); server.close(); },
    }));
  });
}

module.exports = { startRelay };
