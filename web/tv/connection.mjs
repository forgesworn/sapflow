export async function showConnection(document,location,fetchImpl=fetch) {
  const field=(name,value)=>{const element=document.querySelector(`#viewer-${name}`);if(element)element.textContent=value;};
  if(['127.0.0.1','localhost'].includes(location.hostname)) {
    field('ip','Local private preview');field('location','External connection lookup disabled');field('network','Not looked up');return;
  }
  try {
    const response=await fetchImpl('/tv/connection',{credentials:'omit',cache:'no-store',signal:AbortSignal.timeout(5000)});
    if(!response.ok)throw Error('Unavailable');
    const data=await response.json();
    field('ip',typeof data.ip==='string'?data.ip.slice(0,64):'Not reported');
    field('location',[data.city,data.region,data.country].filter(v=>typeof v==='string'&&v.length<=100).join(', ')||'Not reported');
    field('network',(typeof data.network==='string'?data.network.slice(0,100):'Not reported')+(Number.isSafeInteger(data.asn)?` (AS${data.asn})`:''));
  } catch {field('ip','Connection details unavailable');field('location','Not reported');field('network','Not reported');}
}
