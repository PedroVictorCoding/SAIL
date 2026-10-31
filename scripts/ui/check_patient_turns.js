// Run from repository root: gjs scripts/ui/check_patient_turns.js
const GLib=imports.gi.GLib;
const [,bytes]=GLib.file_get_contents('crates/sail-dialogue/web/voice.js');
const source=new TextDecoder().decode(bytes);
const elements=new Map();
const ctx={clearRect(){},beginPath(){},arc(){},fill(){},moveTo(){},lineTo(){},closePath(){},createRadialGradient(){return {addColorStop(){}};}};
function node(id){
  if (!elements.has(id)) elements.set(id,{width:360,height:360,textContent:'',value:'',disabled:false,hidden:false,style:{},dataset:{},files:[],
    classList:{add(){},remove(){},toggle(){}},setAttribute(){},addEventListener(){},getContext(){return ctx;},
    querySelector(){return node(`${id}-child`);},replaceChildren(){},checkValidity(){return true;},reportValidity(){}});
  return elements.get(id);
}
const document={hidden:false,getElementById:node,querySelectorAll(){return [];},addEventListener(){},documentElement:{style:{setProperty(){}}}};
let socket;
class Socket {static OPEN=1;constructor(){this.readyState=1;this.sent=[];socket=this;}send(message){this.sent.push(JSON.parse(message));}}
const window={matchMedia(){return {matches:true};},addEventListener(){}};
const exported=new Function('document','window','location','WebSocket','requestAnimationFrame','setTimeout','clearTimeout',`${source}\nreturn {
 receive(data){ws.onmessage({data:JSON.stringify(data)});},
 state(){return {captureHeld,orbState,turnInProgress};},
 configure:setTurnDetection,
 submit(){sendAction('end_turn');}
};`)(document,window,{protocol:'http:',host:'localhost:8088'},Socket,()=>0,()=>0,()=>{});
function assert(condition,message){if(!condition)throw new Error(message);}
exported.receive({type:'control',control:{is_muted:false,profile:'balanced',turn_detection:{mode:'manual',pause_ms:3000},max_recording_ms:180000}});
assert(node('turnModeSelect').value==='manual','server turn mode must be reflected');
assert(node('turnPauseInput').disabled,'manual mode must disable the automatic pause field');
exported.receive({type:'capture_limit',message:'Your audio is held.'});
assert(exported.state().captureHeld,'limit must hold the recording');
assert(node('micBtn').disabled && !node('endTurnBtn').hidden,'held recording must expose send without allowing more capture');
assert(socket.sent.length===0,'recording limit must not automatically send or split the turn');
exported.receive({type:'telemetry',state:'idle',confidence:0,rms:0,phase:0,spectrum:Array(8).fill(0),captured_ms:180000,vad_us:0,vad_roundtrip_us:0});
assert(exported.state().orbState==='holding','late telemetry must not erase the held recording state');
exported.submit();assert(socket.sent[0].action==='end_turn','explicit send must commit the turn');
exported.receive({type:'turn_committed'});
assert(!exported.state().captureHeld && exported.state().turnInProgress,'commit must release hold and enter transcription');
assert(exported.state().orbState==='transcribing' && !node('micBtn').disabled,'commit must restore microphone controls');
node('turnModeSelect').value='automatic';node('turnPauseInput').value='5';exported.configure();
assert(socket.sent[1].settings.pause_ms===5000,'pause settings must convert seconds to milliseconds');
print('Manual turn, capture-limit, explicit-send and settings checks passed');
