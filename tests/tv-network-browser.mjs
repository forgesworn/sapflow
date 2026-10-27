// Generated programme and synthetic connection data only. No public networking.
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {readFile} from 'node:fs/promises';
import {resolve} from 'node:path';
const base=new URL(process.argv[2]);assert.equal(base.hostname,'127.0.0.1');
const {chromium}=await import(pathToFileURL(process.env.SAPFLOW_PLAYWRIGHT_MODULE).href);
const browser=await chromium.launch({channel:'chrome',headless:true,args:['--autoplay-policy=no-user-gesture-required','--disable-background-networking']});
let state='live',address='198.51.100.7',mediaRequests=0,metadataRequests=0;
try{
 const context=await browser.newContext({serviceWorkers:'block'}),page=await context.newPage(),errors=[];
 page.on('pageerror',e=>errors.push(e.message));
 const observation=()=>({ip:address,city:address==='198.51.100.7'?'Initial city':'Current city',network:'Generated network',scope:'synthetic connection observation',cookieNames:[],authorizationPresent:false});
 await context.route('**/*',async route=>{
  const u=new URL(route.request().url());assert.equal(u.origin,'https://viewer.example','all browser requests stay on the synthetic origin');
  if(u.pathname==='/favicon.ico')return route.fulfill({status:204,body:''});
  if(u.pathname==='/tv/audience')return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({activePlayers:1,peerEnabledPlayers:0,channel:{state},playerVersion:'2026.10.27.6'})});
  if(u.pathname==='/tv/connection'){metadataRequests++;return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(observation())});}
  if(u.pathname.startsWith('/tv/live/event-tv/')){mediaRequests++;const response=await context.request.get(new URL(u.pathname,base).href);return route.fulfill({response});}
  assert(u.pathname.startsWith('/tv/')&&!u.pathname.includes('..'));const name=u.pathname==='/tv/'?'index.html':u.pathname.slice(4);
  const file=resolve('web/tv',name);assert(file.startsWith(resolve('web/tv')+'/'));let body=await readFile(file);
  if(name==='index.html')body=Buffer.from(body.toString().replace('</head>',`<meta name="tv-connection" content='${JSON.stringify(observation()).replace(/'/g,'&#39;')}'>\n</head>`));
  const mime=name.endsWith('.mjs')?'text/javascript':name.endsWith('.css')?'text/css':name.endsWith('.png')?'image/png':name.endsWith('.mp4')?'video/mp4':'text/html';return route.fulfill({status:200,contentType:mime,body});
 });
 await page.goto('https://viewer.example/tv/');assert.equal(await page.locator('#viewer-ip').textContent(),'198.51.100.7');await page.locator('#watch').click();await page.waitForFunction(()=>document.querySelector('#indicator').dataset.state==='playing',null,{timeout:45000});
 const source=await page.locator('#video').evaluate(v=>v.currentSrc);await page.waitForTimeout(2000);
 await context.setOffline(true);await page.waitForTimeout(700);await context.setOffline(false);
 address='203.0.113.9';await page.locator('#refresh-connection').click();await page.waitForFunction(()=>document.querySelector('#viewer-ip').textContent==='203.0.113.9');assert.equal(await page.locator('#viewer-location').textContent(),'Current city');assert(await page.locator('#connection-change').isVisible());
 await page.waitForTimeout(3000);assert.equal(await page.locator('#video').evaluate(v=>v.currentSrc),source,'a brief route interruption preserves the MediaSource');assert.equal(await page.locator('#recovery-count').textContent(),'0');
 state='ended';await page.waitForFunction(()=>document.querySelector('#indicator').dataset.state==='ended',null,{timeout:60000});
 const endedMedia=mediaRequests;state='live';await page.waitForFunction(()=>document.querySelector('#indicator').dataset.state==='available',null,{timeout:15000});
 assert.match(await page.locator('#standby-title').textContent(),/back on air/);assert(await page.locator('#video').evaluate(v=>v.paused));assert.equal(mediaRequests,endedMedia,'new-broadcast notification does not autoplay');
 await page.locator('#start-overlay').click();await page.waitForFunction(()=>document.querySelector('#indicator').dataset.state==='playing',null,{timeout:45000});assert(mediaRequests>endedMedia);assert.deepEqual(errors,[]);
 console.log(JSON.stringify({networkChangeRetainedBuffer:true,metadataRefreshed:metadataRequests>0,newBroadcastAnnounced:true,explicitRejoinPlayed:true,externalNetworkRequests:0}));
}finally{await browser.close();}
