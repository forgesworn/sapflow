import {timingSafeEqual} from 'node:crypto';
export function controlMessage(value){
 if(!value||typeof value!=='object'||Array.isArray(value)||Object.keys(value).sort().join(',')!=='action,session,startedAt'||!['start','end'].includes(value.action)||typeof value.session!=='string'||!/^[a-f0-9]{32}$/.test(value.session)||!Number.isSafeInteger(value.startedAt)||value.startedAt<1)throw Object.assign(Error('Invalid lifecycle notice'),{status:400});
 return value;
}
export function storedControl(value){
 if(!value||typeof value!=='object'||Array.isArray(value)||Object.keys(value).sort().join(',')!=='endedAt,session,startedAt'||typeof value.session!=='string'||!/^[a-f0-9]{32}$/.test(value.session)||!Number.isSafeInteger(value.startedAt)||value.startedAt<1||value.endedAt!==null&&(!Number.isSafeInteger(value.endedAt)||value.endedAt<1))throw Error('Invalid stored lifecycle state');
 return value;
}
export class ChannelControl{
 constructor(token,channel,now=Date.now){if(token!==undefined&&(typeof token!=='string'||!/^[a-f0-9]{64}$/.test(token)))throw Error('Invalid private channel control token');this.token=token?Buffer.from(token,'hex'):null;this.channel=channel;this.now=now;this.session=null;this.startedAt=0;}
 authorised(header){return !!this.token&&typeof header==='string'&&/^Bearer [a-f0-9]{64}$/.test(header)&&timingSafeEqual(this.token,Buffer.from(header.slice(7),'hex'));}
 snapshot(){return this.session?{session:this.session,startedAt:this.startedAt,endedAt:this.channel.explicitEnd?this.channel.endedAt:null}:null;}
 restore(value){const state=storedControl(value);this.session=state.session;this.startedAt=state.startedAt;if(state.endedAt!==null)this.channel.end(state.endedAt);}
 update(value){const message=controlMessage(value);
  if(message.action==='start'){
   if(Math.abs(this.now()-message.startedAt)>120000||message.startedAt<this.startedAt)throw Object.assign(Error('Stale lifecycle notice'),{status:409});
   if(message.session===this.session&&message.startedAt!==this.startedAt)throw Object.assign(Error('Wrong channel session'),{status:409});
   if(message.session!==this.session){this.session=message.session;this.startedAt=message.startedAt;this.channel.begin();}
  }else{if(this.session===null){this.session=message.session;this.startedAt=message.startedAt;}if(message.session!==this.session||message.startedAt!==this.startedAt)throw Object.assign(Error('Wrong channel session'),{status:409});this.channel.end();}
 }
}
