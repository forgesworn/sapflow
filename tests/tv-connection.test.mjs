import test from 'node:test';
import assert from 'node:assert/strict';
import worker,{connectionDetails} from '../edge/tv-connection.mjs';
function request(url='https://receiver.example/tv/connection',options={}){const r=new Request(url,{headers:{'CF-Connecting-IP':'198.51.100.7'},...options});Object.defineProperty(r,'cf',{value:{city:'Example city',region:'Example region',country:'GB',asOrganization:'Example network',asn:64512}});return r;}
test('returns own edge metadata with no identity or street address inference',async()=>{
 const r=await worker.fetch(request());const data=await r.json();assert.equal(data.ip,'198.51.100.7');assert.match(data.locationAccuracy,/approximate/);assert.equal(data.vpnVerified,false);assert(!('address' in data));assert(!('name' in data));assert.match(r.headers.get('cache-control'),/no-store/);assert(!r.headers.has('access-control-allow-origin'));
});
test('cross-site and unsupported requests receive no connection metadata',async()=>{
 for(const r of [request(undefined,{method:'POST'}),request(undefined,{headers:{'Sec-Fetch-Site':'cross-site'}}),request('https://receiver.example/other')]){const response=await worker.fetch(r);assert(response.status>=400);assert(!(await response.text()).includes('198.51.100.7'));}
});
test('missing or malformed fields never imply a precise location or a verified VPN',()=>{
 const r=new Request('https://receiver.example/tv/connection',{headers:{'CF-Connecting-IP':'not an IP'}});const data=connectionDetails(r);assert.equal(data.ip,null);assert.equal(data.city,null);assert.equal(data.vpnVerified,false);
});

test('trusted local preview performs no external connection lookup',async()=>{
 const {showConnection}=await import('../web/tv/connection.mjs');const fields=new Map();const doc={querySelector:key=>{if(!fields.has(key))fields.set(key,{textContent:''});return fields.get(key);}};let calls=0;
 await showConnection(doc,{hostname:'127.0.0.1'},()=>{calls++;});assert.equal(calls,0);assert.match(fields.get('#viewer-ip').textContent,/Local private preview/);
});
test('connection UI treats edge fields as plain text and makes no VPN inference',async()=>{
 const {showConnection}=await import('../web/tv/connection.mjs');const fields=new Map();const doc={querySelector:key=>{if(!fields.has(key))fields.set(key,{textContent:''});return fields.get(key);}};
 await showConnection(doc,{hostname:'receiver.example'},async()=>({ok:true,json:async()=>({ip:'198.51.100.7',city:'<img onerror=bad>',network:'Example VPN',asn:64512})}));
 assert.equal(fields.get('#viewer-location').textContent,'<img onerror=bad>');assert.equal(fields.get('#viewer-network').textContent,'Example VPN (AS64512)');
});

test('metadata reflects identifying header presence without returning secret values',()=>{
 const r=request(undefined,{headers:{Cookie:'session=secret-token; other=another-secret',Authorization:'Bearer private-token',Referer:'https://referrer.example/watch?secret=hidden','User-Agent':'Example browser','Accept-Language':'en-GB'}});
 const data=connectionDetails(r,'page navigation'),json=JSON.stringify(data);
 assert.deepEqual(data.cookieNames,['session','other']);assert(data.authorizationPresent);assert.equal(data.referrer,'https://referrer.example/watch');assert(data.referrerQuery);assert.equal(data.scope,'page navigation');
 for(const secret of ['secret-token','another-secret','private-token','hidden'])assert(!json.includes(secret));
});
test('navigation metadata is escaped for HTML and bounds unreliable location fields',async()=>{
 const {connectionMeta}=await import('../edge/tv-connection.mjs');const meta=connectionMeta({browser:'"><script>bad()</script>&\''});
 assert(!meta.includes('<script>'));assert(meta.includes('&lt;script&gt;'));assert(meta.includes('&quot;'));
 const r=request();const raw=new Request(r);Object.defineProperty(raw,'cf',{value:{latitude:'999',longitude:'-999',clientTcpRtt:-1}});
 const d=connectionDetails(raw);assert.equal(d.latitude,null);assert.equal(d.longitude,null);assert.equal(d.roundTripMs,null);
});
test('navigation receipt takes precedence over a different diagnostic request',async()=>{
 const {showConnection}=await import('../web/tv/connection.mjs');const fields=new Map();let calls=0;
 const doc={querySelector:key=>{if(key==='meta[name="tv-connection"]')return {content:JSON.stringify({ip:'198.51.100.7',scope:'page navigation',cookieNames:['session'],authorizationPresent:true})};if(!fields.has(key))fields.set(key,{textContent:''});return fields.get(key);}};
 await showConnection(doc,{hostname:'receiver.example'},()=>{calls++;});assert.equal(calls,0);assert.match(fields.get('#viewer-cookies').textContent,/session/);assert.match(fields.get('#viewer-scope').textContent,/page navigation/);
});

test('connection observations refresh after a network change instead of reusing navigation',async()=>{
 const {createConnectionMonitor}=await import('../web/tv/connection.mjs');class Field extends EventTarget{textContent='';hidden=true;disabled=false;}
 const fields=new Map(),doc=new EventTarget();doc.querySelector=key=>{if(key==='meta[name="tv-connection"]')return {content:JSON.stringify({ip:'198.51.100.7',city:'Initial city',scope:'page navigation'})};if(!fields.has(key))fields.set(key,new Field());return fields.get(key);};
 const events=new EventTarget(),timers=new Map();let n=0,calls=0,changed=0,options;
 const monitor=createConnectionMonitor({document:doc,location:{hostname:'receiver.example'},events,schedule:fn=>{timers.set(++n,fn);return n;},cancel:id=>timers.delete(id),onAddressChange:()=>changed++,fetchImpl:async(_url,o)=>{calls++;options=o;return {ok:true,json:async()=>({ip:'203.0.113.9',city:'Current city',scope:'diagnostics request'})};}});
 await new Promise(r=>setImmediate(r));assert.equal(calls,0);assert.equal(fields.get('#viewer-ip').textContent,'198.51.100.7');
 events.dispatchEvent(new Event('online'));await new Promise(r=>setImmediate(r));assert.equal(calls,1);assert.equal(fields.get('#viewer-ip').textContent,'203.0.113.9');assert.equal(fields.get('#viewer-location').textContent,'Current city');assert.equal(changed,1);assert.equal(options.credentials,'same-origin');assert.equal(fields.get('#connection-change').hidden,false);
 monitor.destroy();assert.equal(timers.size,0);events.dispatchEvent(new Event('online'));await new Promise(r=>setImmediate(r));assert.equal(calls,1);
});
test('private connection monitor never schedules external address checks',async()=>{
 const {createConnectionMonitor}=await import('../web/tv/connection.mjs');const fields=new Map();const doc={querySelector:key=>{if(!fields.has(key))fields.set(key,{textContent:''});return fields.get(key);}};let calls=0,timers=0;
 const monitor=createConnectionMonitor({document:doc,location:{hostname:'127.0.0.1'},events:new EventTarget(),fetchImpl:()=>calls++,schedule:()=>timers++});await monitor.refresh();monitor.destroy();assert.equal(calls,0);assert.equal(timers,0);
});
