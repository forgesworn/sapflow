// Sapflow's bounded, complete-segment H.264/AAC playlist subset. MIT.
const NAME=/^[a-f0-9]{12}_(?:init[0-9]{0,20}|seg[0-9]{1,20}|part[0-9]{1,20})\.mp4$/;
const allowed=new Set(['#EXTM3U','#EXT-X-VERSION','#EXT-X-INDEPENDENT-SEGMENTS','#EXT-X-TARGETDURATION',
  '#EXT-X-SERVER-CONTROL','#EXT-X-PART-INF','#EXT-X-MEDIA-SEQUENCE','#EXT-X-DISCONTINUITY-SEQUENCE',
  '#EXT-X-MAP','#EXT-X-PART','#EXT-X-PRELOAD-HINT','#EXTINF','#EXT-X-GAP','#EXT-X-PROGRAM-DATE-TIME',
  '#EXT-X-DISCONTINUITY','#EXT-X-ENDLIST','#EXT-X-START']);
function lines(text) {
  if(typeof text!=='string'||text.length>65536||!text.startsWith('#EXTM3U\n')||/[^\x20-\x7e\n]/.test(text)) throw Error('Invalid playlist framing');
  return text.split('\n').filter(Boolean);
}
function number(value,maximum=Number.MAX_SAFE_INTEGER) {
  if(!/^[0-9]+(?:\.[0-9]+)?$/.test(value)) throw Error('Invalid playlist number');
  const n=Number(value);if(!Number.isFinite(n)||n<0||n>maximum)throw Error('Playlist number outside bounds');return n;
}
function uri(line) {
  const matches=[...line.matchAll(/(?:[:,])URI="([^"\n]+)"/g)];
  if(matches.length!==1||line.split('URI').length!==2||!NAME.test(matches[0][1]))throw Error('Unsafe media URI');
  return matches[0][1];
}
export function parseMaster(text) {
  const rows=lines(text);let codec=null,bitrate=0,averageBitrate=0,manifest=null;
  for(const line of rows) {
    if(line.startsWith('#EXT-X-STREAM-INF:')) {
      if(codec)throw Error('Multiple renditions are unsupported');
      codec=/(?:[:,])CODECS="(avc1\.[a-fA-F0-9]{6},mp4a\.40\.2)"/.exec(line)?.[1];
      if(!codec)throw Error('Unsupported channel codecs');
      bitrate=number(/(?:[:,])BANDWIDTH=([0-9]+)/.exec(line)?.[1]??'',1000000000);
      averageBitrate=number(/(?:[:,])AVERAGE-BANDWIDTH=([0-9]+)/.exec(line)?.[1]??'0',1000000000);
    } else if(line==='stream.m3u8') {if(manifest)throw Error('Duplicate variant');manifest=line;}
    else if(!['#EXTM3U','#EXT-X-INDEPENDENT-SEGMENTS'].includes(line)&&!/^#EXT-X-VERSION:[0-9]+$/.test(line))throw Error('Unsupported master feature');
  }
  if(!codec||!manifest)throw Error('Missing channel variant');
  return {codec,bitrate,averageBitrate,manifest};
}
export function parseMedia(text) {
  let sequence=0,cc=0,init=null,duration=null,gap=false,end=false;const segments=[];
  for(const line of lines(text)) {
    if(line.includes('://')||line.includes('//')||line.includes('\\'))throw Error('External reference refused');
    if(!line.startsWith('#')) {
      if(duration===null||!init||(!NAME.test(line)&&line!=='gap.mp4'))throw Error('Unexpected media resource');
      if(line==='gap.mp4'&&!gap)throw Error('Unmarked gap');
      if(!Number.isSafeInteger(sequence))throw Error('Sequence overflow');
      segments.push({sn:sequence++,cc,init,name:line,duration,gap});duration=null;gap=false;
      if(segments.length>1000)throw Error('Playlist budget exceeded');continue;
    }
    const tag=line.split(':',1)[0];if(!allowed.has(tag))throw Error('Unsupported playlist feature');
    const value=line.slice(tag.length+1);
    if(tag==='#EXT-X-MEDIA-SEQUENCE'||tag==='#EXT-X-DISCONTINUITY-SEQUENCE') {
      const n=number(value);if(!Number.isSafeInteger(n)||segments.length)throw Error('Invalid sequence');
      if(tag==='#EXT-X-MEDIA-SEQUENCE')sequence=n;else cc=n;
    } else if(tag==='#EXT-X-DISCONTINUITY'){cc++;if(!Number.isSafeInteger(cc))throw Error('Discontinuity overflow');}
    else if(tag==='#EXT-X-MAP')init=uri(line);
    else if(tag==='#EXTINF') {if(duration!==null)throw Error('Missing segment');duration=number(value.split(',')[0],3600);if(!duration)throw Error('Empty duration');}
    else if(tag==='#EXT-X-GAP')gap=true;
    else if(tag==='#EXT-X-ENDLIST')end=true;
    else if(tag==='#EXT-X-PART'||tag==='#EXT-X-PRELOAD-HINT')uri(line);
    else if(line.includes('URI'))throw Error('Unexpected URI');
  }
  if(duration!==null||!segments.length)throw Error('Incomplete media playlist');
  return {segments,end};
}
