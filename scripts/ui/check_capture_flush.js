// Run: gjs scripts/ui/check_capture_flush.js
const GLib=imports.gi.GLib;
const [,bytes]=GLib.file_get_contents('crates/sail-dialogue/web/capture.js');
const events=[];let Capture;
class Processor {constructor(){this.port={postMessage(data){events.push(data);}};}}
new Function('AudioWorkletProcessor','sampleRate','registerProcessor',new TextDecoder().decode(bytes))(
  Processor,48000,(_name,type)=>{Capture=type;});
function assert(condition,message){if(!condition)throw new Error(message);}
const capture=new Capture();const output=new Float32Array(300).fill(1);
capture.process([[new Float32Array(300).fill(0.5)]],[[output]]);
assert(events.length===0,'partial capture must stay buffered until a frame or explicit flush');
capture.port.onmessage({data:{action:'flush',request_id:7}});
assert(events[0] instanceof ArrayBuffer && events[0].byteLength===1024,'flush must publish a complete transport frame first');
const frame=new DataView(events[0]);
for(let i=0;i<512;i++)assert(frame.getInt16(i*2,true)===(i<100?16384:0),'flush must preserve samples and zero-pad only the unfilled tail');
assert(events[1].type==='flushed' && events[1].request_id===7,'acknowledgement must follow the audio frame');
assert([...output].every(sample=>sample===0),'microphone monitor output must stay silent');
capture.process([[new Float32Array(1536).fill(-0.25)]],[]);
const next=new DataView(events[2]);
for(let i=0;i<512;i++)assert(next.getInt16(i*2,true)===-8192,'the next turn must not duplicate the previous partial frame');
capture.port.onmessage({data:{action:'flush',request_id:8}});
assert(events.length===4 && events[3].type==='flushed','empty flush must not add a silent audio packet');
print('Capture flush ordering, last-frame preservation and continuity checks passed');
