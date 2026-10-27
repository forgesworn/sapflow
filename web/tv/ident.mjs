export function createIdent(video){
 let objectUrl=null,controller=null,state='idle',timer=null,closed=false;
 const reduce=matchMedia('(prefers-reduced-motion: reduce)').matches||navigator.connection?.saveData;
 async function preload(){if(reduce||objectUrl||controller||closed)return;controller=new AbortController();try{
  const response=await fetch('/tv/loading-ident.mp4',{credentials:'omit',cache:'force-cache',priority:'low',signal:controller.signal});if(!response.ok)return;
  const reader=response.body.getReader(),chunks=[];let size=0;while(true){const {done,value}=await reader.read();if(done)break;size+=value.length;if(size>2*1024*1024){await reader.cancel();throw Error('Ident budget');}chunks.push(value);}
  if(closed)return;objectUrl=URL.createObjectURL(new Blob(chunks,{type:'video/mp4'}));video.src=objectUrl;if(['connecting','offline','ended'].includes(state))void video.play().catch(()=>{});
 }catch{}finally{controller=null;}}
 return {setState(next){state=next;if(timer!==null)clearTimeout(timer);timer=null;
  if(['idle','offline','ended'].includes(next)){timer=setTimeout(()=>void preload(),800);}
  if(['playing','paused','buffering','connecting'].includes(next)&&controller)controller.abort();
  if(objectUrl&&['connecting','offline','ended'].includes(next)&&!reduce)void video.play().catch(()=>{});else video.pause();
 },destroy(){closed=true;if(timer!==null)clearTimeout(timer);controller?.abort();video.pause();video.removeAttribute('src');video.load();if(objectUrl)URL.revokeObjectURL(objectUrl);}};
}
