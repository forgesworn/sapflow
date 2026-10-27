// Reflect this visitor's request metadata only. MIT. No lookup or history.
const text=(value,max=100)=>typeof value==='string'?value.trim().slice(0,max)||null:null;
const number=(value,min,max)=>{const n=typeof value==='string'&&value.trim()?Number(value):value;return typeof n==='number'&&Number.isFinite(n)&&n>=min&&n<=max?n:null;};
export function connectionDetails(request,scope='diagnostics request') {
 const cf=request.cf??{},address=request.headers.get('CF-Connecting-IP'),headers=request.headers;
 const cookieNames=(headers.get('Cookie')??'').split(';').map(s=>s.split('=',1)[0].trim()).filter(Boolean).slice(0,20).map(s=>s.slice(0,64));
 let referrer=null,referrerQuery=false;
 try{const url=new URL(headers.get('Referer'));referrer=url.origin+url.pathname;referrerQuery=!!url.search;}catch{}
 return {ip:address&&address.length<=64&&/^[0-9a-f.:]+$/i.test(address)?address:null,
  city:text(cf.city),region:text(cf.region),country:text(cf.country),postalCode:text(cf.postalCode,24),timezone:text(cf.timezone),
  latitude:number(cf.latitude,-90,90),longitude:number(cf.longitude,-180,180),
  network:text(cf.asOrganization),asn:Number.isSafeInteger(cf.asn)&&cf.asn>0?cf.asn:null,
  browser:text(headers.get('User-Agent'),512),languages:text(headers.get('Accept-Language'),200),
  platform:text(headers.get('Sec-CH-UA-Platform')),mobile:text(headers.get('Sec-CH-UA-Mobile')),brands:text(headers.get('Sec-CH-UA'),256),
  httpProtocol:text(cf.httpProtocol),tlsVersion:text(cf.tlsVersion),tlsCipher:text(cf.tlsCipher),edge:text(cf.colo),
  tlsFingerprint:text(cf.botManagement?.ja4??cf.tlsClientExtensionsSha1,128),roundTripMs:number(cf.clientTcpRtt,0,120000),
  cookieNames,authorizationPresent:headers.has('Authorization'),referrer:text(referrer,512),referrerQuery,
  locationAccuracy:'approximate IP database estimate, not GPS',vpnVerified:false,source:'HTTPS edge',scope};
}
export function connectionMeta(data){const escaped=JSON.stringify(data).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));return `<meta name="tv-connection" content="${escaped}">`;}
const headers={'Content-Type':'application/json; charset=utf-8','Cache-Control':'private, no-store, max-age=0','CDN-Cache-Control':'no-store','Cloudflare-CDN-Cache-Control':'no-store','Pragma':'no-cache','Referrer-Policy':'no-referrer','X-Content-Type-Options':'nosniff','Content-Security-Policy':"default-src 'none'; frame-ancestors 'none'"};
export default {async fetch(request){
 const path=new URL(request.url).pathname;
 if(!['/tv/connection','/tv/'].includes(path))return new Response('Not found',{status:404,headers});
 if(!['GET','HEAD'].includes(request.method))return new Response('Method not allowed',{status:405,headers:{...headers,Allow:'GET, HEAD'}});
 if(path==='/tv/connection'){
  if(request.headers.get('Sec-Fetch-Site')==='cross-site')return new Response('Forbidden',{status:403,headers});
  return new Response(request.method==='HEAD'?null:JSON.stringify(connectionDetails(request)),{headers});
 }
 const upstream=await fetch(request,{cf:{cacheEverything:false,cacheTtlByStatus:{'200-299':-1}}});
 if(!upstream.ok||!upstream.headers.get('Content-Type')?.includes('text/html'))return upstream;
 const response=new Response(upstream.body,upstream);
 for(const name of ['Cache-Control','CDN-Cache-Control','Cloudflare-CDN-Cache-Control','Pragma'])response.headers.set(name,headers[name]);
 response.headers.delete('Content-Length');
 if(request.method==='HEAD')return response;
 return new HTMLRewriter().on('head',{element(element){element.append(connectionMeta(connectionDetails(request,'page navigation')),{html:true});}}).transform(response);
}};
