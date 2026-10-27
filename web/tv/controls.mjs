export function createControls({video,screen,onToggle}){
 const get=id=>screen.querySelector('#'+id),play=get('play-pause'),overlay=get('start-overlay'),mute=get('mute'),volume=get('volume'),full=get('fullscreen'),transport=get('transport');
 const listeners=[];let state='idle';
 const on=(target,event,fn)=>{target.addEventListener(event,fn);listeners.push(()=>target.removeEventListener(event,fn));};
 function update(){
  const playing=!video.paused&&!video.ended;play.dataset.playing=String(playing);play.setAttribute('aria-label',playing?'Pause':'Play');
  play.disabled=state==='connecting';screen.dataset.paused=String(video.paused&&video.readyState>0);
  overlay.setAttribute('aria-label',video.readyState>0?'Resume playback':'Watch live');
  overlay.hidden=playing||['connecting','buffering','offline','ended'].includes(state);
  mute.dataset.muted=String(video.muted||video.volume===0);mute.setAttribute('aria-label',video.muted?'Unmute':'Mute');mute.setAttribute('aria-pressed',String(video.muted));volume.value=String(video.muted?0:video.volume);
  full.setAttribute('aria-label',document.fullscreenElement?'Exit fullscreen':'Enter fullscreen');
 }
 on(play,'click',onToggle);on(overlay,'click',onToggle);
 on(mute,'click',()=>{video.muted=!video.muted;update();});on(volume,'input',()=>{video.volume=Number(volume.value);video.muted=video.volume===0;update();});
 async function fullscreen(){try{if(document.fullscreenElement)await document.exitFullscreen();else if(screen.requestFullscreen)await screen.requestFullscreen();else video.webkitEnterFullscreen?.();}catch{}update();}
 on(full,'click',()=>void fullscreen());full.disabled=!screen.requestFullscreen&&!video.webkitEnterFullscreen;
 on(document,'fullscreenchange',update);
 on(screen,'keydown',event=>{if(event.target.closest('button,input,a,summary'))return;if(event.code==='Space'||event.key.toLowerCase()==='k'){event.preventDefault();onToggle();}else if(event.key.toLowerCase()==='m'){video.muted=!video.muted;update();}else if(event.key.toLowerCase()==='f'){event.preventDefault();void fullscreen();}});
 for(const event of ['play','playing','pause','ended','emptied','volumechange','loadedmetadata'])on(video,event,update);
 video.volume=Number(volume.value);video.controls=false;transport.hidden=false;update();
 return {setState(value){state=value;screen.dataset.state=value;update();},destroy(){for(const remove of listeners)remove();video.controls=true;}};
}
