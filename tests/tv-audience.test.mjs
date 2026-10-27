import test from 'node:test';
import assert from 'node:assert/strict';
import {Audience,ChannelState,heartbeat} from '../tools/tv-relayswarm/audience.mjs';
import {createAudience} from '../web/tv/audience.mjs';
const presence=(id='a',values={})=>({id:id.repeat(32),active:true,peers:false,version:'1',...values});
const settle=()=>new Promise(r=>setImmediate(r));
test('audience counts active sessions once, expires them and keeps no network history',()=>{
 let now=0;const audience=new Audience({now:()=>now,ttl:35000,capacity:2});
 audience.update(presence());audience.update(presence());audience.update(presence('b',{peers:true}));
 assert.equal(audience.snapshot().activePlayers,2);assert.equal(audience.snapshot().peerEnabledPlayers,1);
 assert.throws(()=>audience.update(presence('c')),/busy/);assert.deepEqual(Object.keys(audience.players.values().next().value).sort(),['at','peers','version']);
 audience.update(presence('a',{active:false}));assert.equal(audience.snapshot().activePlayers,1);
 now=35000;assert.equal(audience.snapshot().activePlayers,0);
});
test('presence rejects arbitrary fields, identifiers and malformed JSON shapes',()=>{
 for(const value of [null,[],{},presence('x'),presence('a',{ip:'198.51.100.1'}),presence('a',{active:1}),presence('a',{version:'a'.repeat(33)})])assert.throws(()=>heartbeat(value));
});
test('channel distinguishes never on air, temporary interruption and a confirmed end',()=>{
 let now=1;const state=new ChannelState(()=>now),parsed={segments:[{name:'one',cc:0}],end:false};
 state.unavailable(404);assert.equal(state.snapshot().state,'offair');state.observe(parsed);assert.equal(state.snapshot().state,'live');
 now=7000;assert.equal(state.snapshot().state,'reconnecting');state.unavailable(503);assert.equal(state.snapshot().endedAt,null);
 now=8000;state.observe({...parsed,segments:[{name:'two',cc:0}]});assert.equal(state.snapshot().state,'live');
 state.unavailable(404);assert.equal(state.snapshot().state,'ended');assert.equal(state.snapshot().endedAt,8000);
 now=9000;state.observe({...parsed,segments:[{name:'new',cc:0}]});assert.equal(state.snapshot().startedAt,9000);
 state.observe({...parsed,segments:[{name:'new',cc:0}],end:true});assert.equal(state.snapshot().state,'ended');
});
test('private preview only reads aggregates through its local proxy',async()=>{
 const calls=[],snapshots=[];const client=createAudience({location:new URL('http://127.0.0.1/tv/'),fetchImpl:async(url,options)=>{calls.push({url,options});return {ok:true,json:async()=>({activePlayers:3})};},onSnapshot:data=>snapshots.push(data),schedule:()=>1,cancel:()=>{}});
 client.refresh();client.set({watching:true,active:true,peers:false});await settle();client.destroy();
 assert(calls.length>=1);assert(calls.every(c=>c.url==='/tv/audience'&&!c.options.method&&!c.options.body));assert.equal(snapshots.at(-1).activePlayers,3);
});
test('public presence uses a temporary page identifier, unregisters pause and stops polling',async()=>{
 const calls=[],timers=new Map();let n=0;const client=createAudience({location:new URL('https://receiver.example/tv/'),fetchImpl:async(url,options)=>{calls.push(options);return {ok:true,json:async()=>({})};},schedule:fn=>{timers.set(++n,fn);return n;},cancel:id=>timers.delete(id)});
 client.set({watching:true,active:true,peers:true});await settle();client.set({watching:true,active:false,peers:true});await settle();
 client.set({watching:false,active:false,peers:false});await settle();assert.equal(timers.size,0);client.destroy();
 const data=calls.map(o=>JSON.parse(o.body));assert.equal(data[0].active,true);assert.equal(data[1].active,false);assert.match(data[0].id,/^[a-f0-9]{32}$/);assert(data.every(p=>p.id===data[0].id));assert(calls.every(o=>o.credentials==='omit'));
});
