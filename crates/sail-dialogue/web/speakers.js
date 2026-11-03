'use strict';
(() => {
  const root=document.getElementById('speakerControls');
  const status=document.getElementById('speakerStatus');
  const results=document.getElementById('speakerResults');
  let pending=null;
  const element=(tag,text)=>{const node=document.createElement(tag);node.textContent=text;return node;};
  async function api(body,signal) {
    const token=await getOrFetchOwnerToken();
    const response=await fetch('/api/speakers/request',{method:'POST',cache:'no-store',headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json'},body:JSON.stringify(body),signal});
    const data=await response.json();if(!response.ok)throw new Error(data.error||'Speaker request failed.');return data;
  }
  async function clip() {
    const file=document.getElementById('speakerAudio').files[0];if(!file)throw new Error('Choose a WAV clip first.');
    if(file.size>2_000_000)throw new Error('Use a WAV clip of at most 60 seconds.');
    const bytes=new Uint8Array(await file.arrayBuffer());const view=new DataView(bytes.buffer);const text=(start,n)=>String.fromCharCode(...bytes.slice(start,start+n));
    if(bytes.length<44||text(0,4)!=='RIFF'||text(8,4)!=='WAVE'||view.getUint32(4,true)+8!==bytes.length)throw new Error('Use a complete PCM WAV file.');
    let format=false,pcm=null;
    for(let offset=12;offset+8<=bytes.length;) {
      const size=view.getUint32(offset+4,true),start=offset+8;if(start+size>bytes.length)throw new Error('Truncated WAV file.');
      if(text(offset,4)==='fmt ') {if(size<16||view.getUint16(start,true)!==1||view.getUint16(start+2,true)!==1||view.getUint32(start+4,true)!==16000||view.getUint16(start+14,true)!==16)throw new Error('Use 16 kHz mono PCM16 WAV audio.');format=true;}
      if(text(offset,4)==='data')pcm=bytes.slice(start,start+size);
      offset=start+size+(size%2);
    }
    if(!format||!pcm||pcm.length<32000||pcm.length>1920000||pcm.length%2)throw new Error('Use 1–60 seconds of 16 kHz mono PCM16 audio.');
    let binary='';for(let offset=0;offset<pcm.length;offset+=16384)binary+=String.fromCharCode(...pcm.subarray(offset,offset+16384));return btoa(binary);
  }
  async function run(action,extra={}) {
    if(pending)return;
    pending=new AbortController();root.querySelectorAll('button[data-speaker-run]').forEach(b=>b.disabled=true);document.getElementById('speakerCancel').hidden=false;status.textContent='Analyzing locally…';results.replaceChildren();
    try {
      const pcm=await clip();const data=await api({action,pcm,...extra},pending.signal);
      if(data.enrolled) {status.textContent=`Enrolled ${data.name}. Audio was not retained.`;await profiles();}
      else {status.textContent='Analysis complete. Voice matches are advisory, not identity proof.';
        if(!data.segments.length)results.append(element('li','No speech detected.'));
        for(const segment of data.segments) {const match=data.matches.find(m=>m.speaker===segment.speaker);const who=match?.name||'Unknown';const score=match?.similarity==null?'':` · similarity ${match.similarity.toFixed(2)}`;results.append(element('li',`${(segment.start_ms/1000).toFixed(2)}–${(segment.end_ms/1000).toFixed(2)}s · ${segment.speaker} · ${who} · ${match?.decision||'unknown'}${score}`));}
      }
    }catch(error){status.textContent=error.name==='AbortError'?'Request cancelled. Refresh profiles to check any enrollment already committed.':error.message;}
    finally {pending=null;root.querySelectorAll('button[data-speaker-run]').forEach(b=>b.disabled=false);document.getElementById('speakerCancel').hidden=true;}
  }
  async function profiles() {
    const data=await api({action:'list'});const list=document.getElementById('speakerProfiles');list.replaceChildren();
    if(!data.profiles.length)list.append(element('p','No enrolled voices. Anonymous diarization does not require enrollment.'));
    for(const profile of data.profiles) {
      const row=element('div','');row.className='speaker-profile';row.append(element('span',profile.name));
      const verify=element('button','Verify selected clip');verify.type='button';verify.className='quiet-button';verify.dataset.speakerRun='';verify.addEventListener('click',()=>run('verify',{profile_id:profile.id}));
      const remove=element('button','Delete voice profile');remove.type='button';remove.className='quiet-button';remove.addEventListener('click',async()=>{if(!confirm(`Delete the enrolled voice profile for ${profile.name}?`))return;try {await api({action:'delete',id:profile.id});await profiles();status.textContent='Voice profile deleted.';}catch(error){status.textContent=error.message;}});row.append(verify,remove);list.append(row);
    }
  }
  document.getElementById('speakerAnalyze').addEventListener('click',()=>run('analyze'));
  document.getElementById('speakerEnroll').addEventListener('click',()=>{
    const name=document.getElementById('speakerName').value.trim();const consent=document.getElementById('speakerConsent').checked;
    if(!name||!consent){status.textContent='Provide a name and explicit enrollment consent.';return;}
    run('enroll',{name,consent,request_id:crypto.randomUUID()});
  });
  document.getElementById('speakerRefresh').addEventListener('click',()=>profiles().catch(error=>status.textContent=error.message));
  document.getElementById('speakerCancel').addEventListener('click',()=>pending?.abort());
})();
