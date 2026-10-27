// Show only this request's edge-visible connection data. MIT. No lookup or storage.
const text=value=>typeof value==='string'?value.trim().slice(0,100)||null:null;
export function connectionDetails(request) {
  const cf=request.cf??{},address=request.headers.get('CF-Connecting-IP');
  return {ip:address&&address.length<=64&&/^[0-9a-f.:]+$/i.test(address)?address:null,
    city:text(cf.city),region:text(cf.region),country:text(cf.country),
    network:text(cf.asOrganization),asn:Number.isSafeInteger(cf.asn)&&cf.asn>0?cf.asn:null,
    locationAccuracy:'approximate',vpnVerified:false,source:'HTTPS edge'};
}
const headers={'Content-Type':'application/json; charset=utf-8','Cache-Control':'private, no-store, max-age=0',
  'Pragma':'no-cache','Referrer-Policy':'no-referrer','X-Content-Type-Options':'nosniff','Content-Security-Policy':"default-src 'none'; frame-ancestors 'none'"};
export default {
  async fetch(request) {
    if(new URL(request.url).pathname!=='/tv/connection')return new Response('Not found',{status:404,headers});
    if(!['GET','HEAD'].includes(request.method))return new Response('Method not allowed',{status:405,headers:{...headers,Allow:'GET, HEAD'}});
    if(request.headers.get('Sec-Fetch-Site')==='cross-site')return new Response('Forbidden',{status:403,headers});
    return new Response(request.method==='HEAD'?null:JSON.stringify(connectionDetails(request)),{headers});
  }
};
