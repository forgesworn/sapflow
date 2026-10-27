export async function showConnection(document,location,fetchImpl=fetch,{navigation=true}={}) {
 const field=(name,value)=>{const element=document.querySelector(`#viewer-${name}`);if(element)element.textContent=value??'Not supplied';};
 if(['127.0.0.1','localhost','[::1]'].includes(location.hostname)){
  for(const name of ['ip','location','network','postal','coordinates','timezone','browser','languages','platform','transport','fingerprint','cookies','auth','referrer','edge','scope'])field(name,'Not looked up in private preview');
  field('ip','Local private preview');field('location','External connection lookup disabled');return;
 }
 try{
  let data;const meta=document.querySelector('meta[name="tv-connection"]');
  if(navigation&&typeof meta?.content==='string'&&meta.content.length<=8192)data=JSON.parse(meta.content);
  else{const response=await fetchImpl('/tv/connection',{credentials:'same-origin',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(5000)});if(!response.ok)throw Error('Unavailable');data=await response.json();}
  const value=(name,max=512)=>typeof data[name]==='string'?data[name].slice(0,max):null;
  field('ip',value('ip',64));field('location',[data.city,data.region,data.country].filter(v=>typeof v==='string'&&v.length<=100).join(', ')||'Not supplied');
  field('network',(value('network',100)??'Not supplied')+(Number.isSafeInteger(data.asn)?` (AS${data.asn})`:''));
  field('postal',value('postalCode',24)?`${value('postalCode',24)} · IP estimate`:null);
  field('coordinates',Number.isFinite(data.latitude)&&Number.isFinite(data.longitude)?`${data.latitude.toFixed(2)}, ${data.longitude.toFixed(2)} · approximate, not GPS`:null);
  field('timezone',value('timezone'));field('browser',value('browser'));field('languages',value('languages',200));
  field('platform',[value('platform'),value('brands',256),value('mobile')?`mobile hint ${value('mobile')}`:null].filter(Boolean).join(' · ')||null);
  field('transport',[value('httpProtocol'),value('tlsVersion'),value('tlsCipher')].filter(Boolean).join(' · ')||null);
  field('fingerprint',value('tlsFingerprint',128));field('cookies',Array.isArray(data.cookieNames)?data.cookieNames.filter(v=>typeof v==='string').slice(0,20).join(', ')||'None received on this request':'Not supplied');
  field('auth',data.authorizationPresent?'Present · value withheld':'Not received on this request');
  field('referrer',value('referrer')?value('referrer')+(data.referrerQuery?' · query received, values withheld':''):'Not received on this request');
  field('edge',value('edge')?`${value('edge')} edge${Number.isFinite(data.roundTripMs)?` · ${Math.round(data.roundTripMs)} ms round trip`:''}`:null);
  field('scope',value('scope')??'This diagnostics request');return data;
 }catch{for(const name of ['ip','location','network','postal','coordinates','timezone','browser','languages','platform','transport','fingerprint','cookies','auth','referrer','edge','scope'])field(name,'Unavailable');}
}


/** Re-observe this page's own connection; no external lookup in private preview. */
export function createConnectionMonitor({document,location,events=globalThis,fetchImpl=fetch,onAddressChange=()=>{},schedule=setTimeout,cancel=clearTimeout}={}){
 const button=document.querySelector('#refresh-connection'),status=document.querySelector('#connection-status'),notice=document.querySelector('#connection-change');
 const local=['127.0.0.1','localhost','[::1]'].includes(location.hostname);
 let closed=false,busy=false,pending=false,timer=null,controller=null,lastAddress=null;
 const listeners=[];const on=(target,event,fn)=>{target?.addEventListener?.(event,fn);listeners.push(()=>target?.removeEventListener?.(event,fn));};
 const say=value=>{if(status)status.textContent=value;};
 async function refresh(navigation=false){
  if(closed)return;if(local){await showConnection(document,location,fetchImpl);say('External connection checks are disabled in private preview.');if(button)button.disabled=true;return;}
  if(busy){pending=true;return;}busy=true;if(timer!==null)cancel(timer);timer=null;controller=new AbortController();if(button)button.disabled=true;
  const fetchCurrent=(url,options)=>fetchImpl(url,{...options,signal:AbortSignal.any([controller.signal,options.signal])});
  try{
   const data=await showConnection(document,location,fetchCurrent,{navigation});if(closed)return;
   if(data){say((navigation?'Page navigation observed':'Connection checked')+' at '+new Date().toLocaleTimeString()+'. IP location is approximate.');
    if(typeof data.ip==='string'){if(lastAddress&&lastAddress!==data.ip){if(notice){notice.hidden=false;notice.textContent='Your visible IP address changed. Check your VPN; peer assistance is switched off if it was active.';}onAddressChange();}lastAddress=data.ip;}
   }else say('Connection check unavailable. These fields are not a current observation.');
  }finally{busy=false;if(button)button.disabled=false;if(!closed){if(pending){pending=false;void refresh();}else timer=schedule(()=>{timer=null;void refresh();},15000);}}
 }
 on(button,'click',()=>void refresh());on(events,'online',()=>void refresh());on(events.navigator?.connection,'change',()=>void refresh());on(document,'visibilitychange',()=>{if(document.visibilityState==='visible')void refresh();});
 void refresh(true);
 return {refresh:()=>refresh(),destroy(){closed=true;if(timer!==null)cancel(timer);controller?.abort();for(const remove of listeners)remove();lastAddress=null;}};
}
