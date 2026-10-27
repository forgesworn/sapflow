import test from 'node:test';
import assert from 'node:assert/strict';
import worker,{connectionDetails} from '../edge/tv-connection.mjs';
function request(url='https://receiver.example/tv/connection',options={}){const r=new Request(url,{headers:{'CF-Connecting-IP':'198.51.100.7'},...options});Object.defineProperty(r,'cf',{value:{city:'Example city',region:'Example region',country:'GB',asOrganization:'Example network',asn:64512}});return r;}
test('returns own edge metadata with no identity or street address inference',async()=>{
 const r=await worker.fetch(request());const data=await r.json();assert.equal(data.ip,'198.51.100.7');assert.equal(data.locationAccuracy,'approximate');assert.equal(data.vpnVerified,false);assert(!('address' in data));assert(!('name' in data));assert.match(r.headers.get('cache-control'),/no-store/);assert(!r.headers.has('access-control-allow-origin'));
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
