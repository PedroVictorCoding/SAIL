// GJS check of the browser side of model-requested snapshots (crates/sail-dialogue/web/ui.js).
// Run from the repository root: gjs scripts/ui/check_visual_capture.js
const GLib=imports.gi.GLib;
const nodes=new Map();
function node(id){if(!nodes.has(id))nodes.set(id,{id,open:false,hidden:false,textContent:'',addEventListener(){},focus(){focused=id;},showModal(){this.open=true;},close(){this.open=false;},querySelectorAll(){return [];}});return nodes.get(id);}
let sent=[],stopped=0,pickers=[],errors=[],artifacts=0,chatImages=[],deny=false,focused=null;
const liveVideo={videoWidth:1280,videoHeight:720};
const document={getElementById:id=>id==='cameraVideo'||id==='desktopVideo'?liveVideo:node(id),createElement:t=>t==='video'?{videoWidth:1920,videoHeight:1080,play:async()=>{}}:{width:0,height:0,getContext:()=>({drawImage(){}}),toDataURL:()=> 'data:image/jpeg;base64,cGlj'}};
const window={isSecureContext:true,addEventListener(){},AdaptiveFeedEngine:{cameraActive:false,desktopActive:false}};
const stream={getTracks:()=>[{stop(){stopped++;}}]};
const navigator={mediaDevices:{getDisplayMedia:async()=>{pickers.push('screen');if(deny)throw {name:'NotAllowedError'};return stream;},getUserMedia:async()=>{pickers.push('camera');return stream;}}};
function updateState(){} function renderArtifact(){artifacts++;} function showError(e){errors.push(e);} function sendAction(action,data){sent.push({action,...data});return true;} function setTimeout(){} function appendConversationImage(role,url,caption){chatImages.push({role,url,caption});}
function check(ok,what){if(!ok)throw new Error('Capture assertion failed: '+what);}
const source=imports.byteArray.toString(GLib.file_get_contents('crates/sail-dialogue/web/ui.js')[1]);
const start=source.indexOf('// Model-requested snapshots');
const end=source.indexOf('// Auto-ping live metrics');
check(start>=0&&end>start,'capture section markers');
const loop=new GLib.MainLoop(null,false);
eval(source.slice(start,end)+`
(async()=>{
 // Screen not shared live: the dialog asks, preselecting the model's source.
 requestVisualCapture({request_id:7,source:'screen',question:'Which error?'});
 check(node('captureDialog').open && pickers.length===0,'dialog before any picker');
 check(focused==='captureScreenChoice' && node('captureExplanation').textContent.includes('Which error?'),'source preselected, question shown');
 await captureRequestedVisual('screen');
 check(sent[0].action==='snapshot' && sent[0].request_id===7 && sent[0].image.data_base64==='cGlj','snapshot answers the request');
 check(stopped===1 && !node('captureDialog').open && artifacts===1,'stream stopped, dialog closed, image shown');
 check(chatImages.length===1 && chatImages[0].caption==='Screen snapshot' && chatImages[0].url==='data:image/jpeg;base64,cGlj','snapshot also shown in the conversation');

 // Camera already shared live: answered at once from the preview, no dialog or picker.
 window.AdaptiveFeedEngine.cameraActive=true;
 requestVisualCapture({request_id:8,source:'camera'});
 check(sent[1].request_id===8 && sent[1].image && !node('captureDialog').open && pickers.length===1,'live camera frame used without asking');
 check(chatImages.length===2 && chatImages[1].caption==='Camera snapshot','live camera snapshot also shown in the conversation');
 window.AdaptiveFeedEngine.cameraActive=false;

 // Browser denial is reported immediately, without another approval or retry.
 deny=true; requestVisualCapture({request_id:9,source:'screen'});
 await captureRequestedVisual('screen');
 check(sent.length===3 && sent[2].error && errors.length===1 && !node('captureDialog').open,'denial returned to waiting turn');
 check(chatImages.length===2,'denied capture shows nothing in the conversation');
 // Cancel tells the waiting turn the user declined.
 requestVisualCapture({request_id:11,source:'screen'});
 declineVisualCapture();
 check(!node('captureDialog').open && sent[3].request_id===11 && /declined/.test(sent[3].error),'decline reported');
 check(chatImages.length===2,'decline shows nothing in the conversation');

 // A turn ending (cancelVisualCapture) withdraws silently.
 deny=false; requestVisualCapture({request_id:10,source:'camera'}); cancelVisualCapture();
 check(!node('captureDialog').open && sent.length===4,'silent withdrawal');
 check(chatImages.length===2,'silent withdrawal shows nothing in the conversation');
 // Flush the cancelled asynchronous camera acquisition: no stale image, stream stopped.
 for(let i=0;i<12;i++) await Promise.resolve();
 check(sent.length===4 && stopped===2,'cancelled camera stream stopped without sending');
 requestVisualCapture({request_id:12,source:'camera'});
 check(!node('captureDialog').open,'camera starts without SAIL approval');
 for(let i=0;i<12;i++) await Promise.resolve();
 check(sent[4].request_id===12 && sent[4].image && stopped===3,'automatic one-shot camera delivered and stopped');
 window.AdaptiveFeedEngine.desktopActive=true;
 requestVisualCapture({request_id:13,source:'screen'});
 check(sent[5].request_id===13 && !node('captureDialog').open,'live screen captured immediately');
 print('Snapshot request, live-frame, consent, denial, decline, chat preview and cleanup checks passed');
})().catch(e=>{printerr(e.stack||e);imports.system.exit(1);}).finally(()=>loop.quit());`);
loop.run();
