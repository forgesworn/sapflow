const NS='http://www.w3.org/2000/svg';
export function bytes(value){const n=Math.max(0,Number(value)||0);return n>=1048576?`${(n/1048576).toFixed(1)} MiB`:n>=1024?`${(n/1024).toFixed(1)} KiB`:`${Math.round(n)} B`;}
export function renderNetwork(container,metrics={},enabled=false){
 const document=container.ownerDocument,svg=document.createElementNS(NS,'svg');svg.setAttribute('viewBox','0 0 540 190');
 const node=(tag,attributes,text)=>{const n=document.createElementNS(NS,tag);for(const [k,v]of Object.entries(attributes))n.setAttribute(k,String(v));if(text!==undefined)n.textContent=text;svg.append(n);return n;};
 const count=Math.min(4,Math.max(0,metrics.peers?.connected??0));
 const originActive=(metrics.rates?.origin??0)>0,peerActive=(metrics.rates?.peers??0)>0,uploadActive=(metrics.rates?.upload??0)>0;
 node('path',{d:'M100 90H245',class:'link'+(originActive?' flowing':'')});
 node('circle',{cx:80,cy:90,r:24,class:'node'});node('text',{x:80,y:94,'text-anchor':'middle'},'RX');node('text',{x:80,y:137,'text-anchor':'middle'},'Receiver');
 node('circle',{cx:270,cy:90,r:29,class:'you'});node('text',{x:270,y:94,'text-anchor':'middle',class:'you-text'},'YOU');
 node('text',{x:174,y:72,'text-anchor':'middle'},originActive?bytes(metrics.rates.origin)+'/s':'Origin fallback');
 node('text',{x:270,y:145,'text-anchor':'middle'},enabled?'Verified peer assistance':'Direct receiver delivery');
 for(let i=0;i<count;i++){const y=35+i*40,x=455;node('path',{d:`M300 90Q380 90 ${x-18} ${y}`,class:'link'+(peerActive||uploadActive?' flowing':'')});node('circle',{cx:x,cy:y,r:15,class:'node'});node('text',{x:x,y:y+3,'text-anchor':'middle'},String(i+1));node('text',{x:x+25,y:y+3},'Peer '+(i+1));}
 if(!count)node('text',{x:430,y:94,'text-anchor':'middle'},enabled?'Finding peers…':'Peers off');
 if(peerActive||uploadActive)node('text',{x:386,y:176,'text-anchor':'middle'},`${bytes(metrics.rates.peers??0)}/s in · ${bytes(metrics.rates.upload??0)}/s out`);
 container.replaceChildren(svg);container.setAttribute('aria-label',`${count} connected peers; ${bytes(metrics.bytes?.fromPeers??0)} verified peer media received; ${bytes(metrics.bytes?.uploaded??0)} shared. ${enabled?'Receiver fallback available.':'Peer assistance off.'}`);
}
export function createNetwork(document){
 const container=document.querySelector('#swarm-visual');let previous=null;
 const field=(id,value)=>{const node=document.querySelector('#'+id);if(node)node.textContent=value;};
 return {update(metrics={},enabled=false){
  const now=performance.now(),b=metrics.bytes??{};let rates={origin:0,peers:0,upload:0};
  if(previous){const seconds=Math.max(.2,(now-previous.at)/1000);rates={origin:Math.max(0,((b.fromOrigin??0)-previous.origin)/seconds),peers:Math.max(0,((b.fromPeers??0)-previous.peers)/seconds),upload:Math.max(0,((b.uploaded??0)-previous.upload)/seconds)};}
  previous={at:now,origin:b.fromOrigin??0,peers:b.fromPeers??0,upload:b.uploaded??0};
  renderNetwork(container,{...metrics,rates},enabled);field('peer-received',bytes(b.fromPeers));field('peer-shared',bytes(b.uploaded));field('peer-connected',String(metrics.peers?.connected??0));
  field('network-mode',enabled?(metrics.peers?.connected?'Peer assisted':'Looking for peers'):'Receiver only');field('delivery-mode',enabled?'RelaySwarm + receiver':'Receiver');
 },reset(){previous=null;this.update({},false);}};
}
