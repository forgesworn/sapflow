// Anonymous, bounded, in-memory player presence. No IP addresses or history.
export function heartbeat(value){
  if(!value||typeof value!=='object'||Array.isArray(value)||Object.keys(value).sort().join(',')!=='active,id,peers,version'
    ||typeof value.id!=='string'||!/^[a-f0-9]{32}$/.test(value.id)||typeof value.active!=='boolean'||typeof value.peers!=='boolean'
    ||typeof value.version!=='string'||!/^[a-zA-Z0-9._-]{1,32}$/.test(value.version))throw Object.assign(Error('Invalid player presence'),{status:400});
  return value;
}
export class Audience{
  constructor({now=Date.now,ttl=35000,capacity=4096}={}){this.now=now;this.ttl=ttl;this.capacity=capacity;this.players=new Map();}
  prune(){const at=this.now();for(const [id,p]of this.players)if(at-p.at>=this.ttl)this.players.delete(id);}
  update(value){const p=heartbeat(value);this.prune();if(!p.active){this.players.delete(p.id);return;}
    if(!this.players.has(p.id)&&this.players.size>=this.capacity)throw Object.assign(Error('Audience busy'),{status:503});
    this.players.set(p.id,{at:this.now(),peers:p.peers,version:p.version});}
  snapshot(){this.prune();return {activePlayers:this.players.size,peerEnabledPlayers:[...this.players.values()].filter(p=>p.peers).length,updatedAt:this.now(),ttlSeconds:this.ttl/1000};}
}
export class ChannelState{
  constructor(now=Date.now){this.now=now;this.state='offair';this.everLive=false;this.lastMedia=null;this.lastMediaAt=0;this.startedAt=null;this.endedAt=null;this.missingSince=null;this.explicitEnd=false;}
  begin(){this.explicitEnd=false;this.endedAt=null;this.state=this.everLive?'reconnecting':'offair';}
  end(at=this.now()){if(!this.explicitEnd)this.endedAt=at;this.explicitEnd=true;this.state='ended';}
  observe(parsed){if(this.explicitEnd)return;this.missingSince=null;const media=parsed.segments.filter(s=>!s.gap).at(-1);if(!media)return;
    const key=`${media.name}:${media.cc}`;
    if(key!==this.lastMedia){if(!this.everLive||this.state==='ended')this.startedAt=this.now();this.lastMedia=key;this.lastMediaAt=this.now();this.everLive=true;this.endedAt=null;this.state='live';}
    if(parsed.end){this.state='ended';this.endedAt??=this.now();}
  }
  unavailable(status){if(this.explicitEnd)return;if([404,410].includes(status)){this.missingSince??=this.now();const ended=this.everLive&&this.now()-this.missingSince>=2000;this.state=ended?'ended':this.everLive?'reconnecting':'offair';if(ended)this.endedAt??=this.now();}else{this.missingSince=null;this.state=this.everLive?'reconnecting':'offair';}}
  snapshot(){if(this.state==='live'&&this.now()-this.lastMediaAt>6000)this.state='reconnecting';return {state:this.state,startedAt:this.startedAt,endedAt:this.endedAt,lastMediaAgeSeconds:this.everLive?Math.max(0,Math.floor((this.now()-this.lastMediaAt)/1000)):null};}
}
