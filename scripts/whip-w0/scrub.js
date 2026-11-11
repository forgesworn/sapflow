'use strict';
// Scrub an offer or answer for publication: every IP address becomes a
// documentation address (RFC 5737 203.0.113.x for IPv4, RFC 3849 2001:db8::x for
// IPv6), candidate and media ports become 40000+n (9 and 0 are kept: they are
// protocol conventions), and mDNS host names become a placeholder.
// 0.0.0.0 is kept as the unspecified address. Usage: node scrub.js <in> > <out>
const fs = require('node:fs');

function scrub(text) {
  const v4 = new Map(), v6 = new Map(), ports = new Map(), mdns = new Map();
  const m4 = (ip) => { if (ip === '0.0.0.0') return ip; if (!v4.has(ip)) v4.set(ip, `203.0.113.${v4.size + 1}`); return v4.get(ip); };
  const m6 = (ip) => { if (!v6.has(ip)) v6.set(ip, `2001:db8::${v6.size + 1}`); return v6.get(ip); };
  const mp = (p) => { if (p === '9' || p === '0') return p; if (!ports.has(p)) ports.set(p, String(40000 + ports.size)); return ports.get(p); };
  const mm = (n) => { if (!mdns.has(n)) mdns.set(n, `host-${mdns.size + 1}.mdns.invalid`); return mdns.get(n); };
  return text.split(/\r?\n/).map((l) => {
    if (l.startsWith('a=candidate:')) {
      const p = l.split(' ');
      p[4] = /\.[l]ocal$/.test(p[4]) ? mm(p[4]) : p[4].includes(':') ? m6(p[4]) : m4(p[4]);
      p[5] = mp(p[5]);
      return p.join(' ');
    }
    if (l.startsWith('m=')) { const p = l.split(' '); p[1] = mp(p[1]); return p.join(' '); }
    return l.replace(/\b(IN IP4 )(\d+\.\d+\.\d+\.\d+)/g, (_, a, ip) => a + m4(ip))
      .replace(/\b(IN IP6 )([0-9a-f:]+)/gi, (_, a, ip) => a + m6(ip));
  }).join('\r\n');
}

if (require.main === module) process.stdout.write(scrub(fs.readFileSync(process.argv[2], 'utf8')));
module.exports = { scrub };
