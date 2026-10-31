const GLib=imports.gi.GLib;
const nodes=new Map();
function node(id){if(!nodes.has(id))nodes.set(id,{open:false,hidden:false,addEventListener(){},showModal(){this.open=true;},close(){this.open=false;},querySelectorAll(){return [];}});return nodes.get(id);}
let sent=[],stopped=0,pickers=[],errors=[],deny=false;
const document={getElementById:node,createElement:t=>t==='video'?{videoWidth:1920,videoHeight:1080,play:async()=>{}}:{getContext:()=>({drawImage(){}}),toDataURL:()=> 'data:image/jpeg;base64,cGlj'}};
const window={isSecureContext:true,addEventListener(){}};
const stream={getTracks:()=>[{stop(){stopped++;}}]};
const navigator={mediaDevices:{getDisplayMedia:async()=>{pickers.push('screen');if(deny)throw {name:'NotAllowedError'};return stream;},getUserMedia:async()=>{pickers.push('camera');return stream;}}};
function updateState(){} function renderArtifact(){} function showError(e){errors.push(e);} function sendAction(action,data){sent.push({action,...data});return true;} function setTimeout(){}
function check(ok){if(!ok)throw new Error('Capture assertion failed');}
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/ui.js')[1]);
const loop=new GLib.MainLoop(null,false);
eval(source.slice(source.indexOf('// Screen sharing requires'))+`
(async()=>{
 requestVisualCapture({source:'screen',prompt:'Explain my screen'});
 check(pickers.length===0 && node('captureCameraChoice').hidden);
 await captureRequestedVisual('screen');
 check(sent[0].text==='Explain my screen' && sent[0].vision.data_base64==='cGlj' && stopped===1);
 requestVisualCapture({source:'choose',prompt:'Capture an image'});
 check(!node('captureCameraChoice').hidden && !node('captureScreenChoice').hidden);
 await captureRequestedVisual('camera'); check(pickers[1]==='camera' && stopped===2);
 deny=true; requestVisualCapture({source:'screen',prompt:'Screenshot'});
 await captureRequestedVisual('screen'); check(sent.length===2 && errors.length===1 && node('captureDialog').open);
 cancelVisualCapture(); check(!node('captureDialog').open);
 print('Visual capture source, attachment, cleanup and denial checks passed');
})().catch(e=>{printerr(e.stack);imports.system.exit(1);}).finally(()=>loop.quit());`);
loop.run();
