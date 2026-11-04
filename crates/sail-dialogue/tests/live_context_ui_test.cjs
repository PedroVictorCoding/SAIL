const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(`${__dirname}/../web/voice.js`, 'utf8');
function setup(token = async () => 'owner-test-token') {
  const elements = Object.fromEntries(['liveContextBtn','liveContextStatus'].map(id => [id, {
    setAttribute(k,v) { this[k] = v; }, addEventListener(_, f) { this.click = f; }
  }]));
  const actions = [];
  const context = vm.createContext({document:{getElementById:id=>elements[id]},
    getOrFetchOwnerToken:token, showError:error=>actions.push({error}), stopPlayback:()=>{},
    liveContextEnabled:false, ws:{readyState:1}, WebSocket:{OPEN:1},
    sendAction:(action, fields)=>actions.push({action,...fields})});
  vm.runInContext(source.slice(source.indexOf('function renderLiveContext()'), source.indexOf('function renderLearning()')), context);
  return {context, elements, actions};
}
test('listening requires owner token and waits for server state', async () => {
  const {context,elements,actions} = setup();
  context.renderLiveContext();
  await elements.liveContextBtn.click();
  assert.equal(actions[0].action,'set_live_context');
  assert.equal(actions[0].owner_token,'owner-test-token');
  assert.equal(actions[0].enabled,true);
  assert.equal(elements.liveContextBtn['aria-pressed'],'false');
  context.liveContextEnabled = true; context.renderLiveContext();
  assert.match(elements.liveContextStatus.textContent,/type to ask/);
  await elements.liveContextBtn.click();
  assert.equal(actions[1].enabled,false);
  assert.equal(actions[1].owner_token,undefined);
});
test('failed authentication and connection replacement cannot enable capture', async () => {
  let resolve;
  const {context,elements,actions} = setup(()=>new Promise(r=>resolve=r));
  const pending = elements.liveContextBtn.click();
  context.ws = {readyState:1}; resolve('old-session-token'); await pending;
  assert.equal(actions.length,0);
  const denied = setup(async()=>{throw Error('owner denied');});
  await denied.elements.liveContextBtn.click();
  assert.equal(denied.actions[0].error,'owner denied');
  assert.equal(denied.context.liveContextEnabled,false);
});

function captureUI(overrides = {}) {
  const actions = [];
  const context = vm.createContext({
    liveContextEnabled:true, liveContextReplyPending:false,
    pendingAudio:0, playingSources:new Set(),
    clientCaptureSerial:1, clientFrameGeneration:0, clientCapturing:false,
    clientSpeechActive:{stt:true}, isRecording:true, isMuted:false,
    captureHeld:false, turnInProgress:false,
    SailClientSpeech:{resetCapture(){}, takeCapture:()=>new Float32Array(512), transcribe:async()=>({text:'ambient'})},
    sendAction:(action, fields)=>actions.push({action,...fields}),
    setCaptureHeld(){}, updateState(){}, failClientSpeech(error){throw error;},
    ...overrides,
  });
  vm.runInContext(source.slice(source.indexOf('function listeningReplyBusy()'),source.indexOf('function renderLiveContext()')),context);
  vm.runInContext(source.slice(source.indexOf('async function commitClientCapture()'),source.indexOf('// Synthesize a reply fragment')),context);
  return {context,actions};
}
test('revoking capture discards a delayed browser transcript', async () => {
  let resolve;
  const {context,actions} = captureUI({SailClientSpeech:{resetCapture(){},takeCapture:()=>new Float32Array(512),transcribe:()=>new Promise(r=>{resolve=r;})}});
  const pending = context.commitClientCapture();
  context.discardListeningCapture(); resolve({text:'late private speech'}); await pending;
  assert.equal(actions.length,0);
  assert.equal(context.clientFrameGeneration,1);
});
test('ambient client speech carries capture identity and pauses during an explicit reply', async () => {
  const {context,actions} = captureUI();
  await context.commitClientCapture();
  assert.deepEqual(actions,[{action:'voice_text',text:'ambient',capture_id:1}]);
  context.liveContextReplyPending=true;
  await context.commitClientCapture();
  assert.equal(actions.length,1);
  context.liveContextReplyPending=false; context.pendingAudio=1;
  assert.equal(context.listeningReplyBusy(),true);
});
test('queued browser frames from before revocation never reach VAD', async () => {
  const {context} = captureUI({SailClientSpeech:{frame(){throw Error('stale frame reached VAD');}}});
  vm.runInContext(source.slice(source.indexOf('async function handleClientFrame('),source.indexOf('// Transcribe the captured turn')),context);
  context.clientFrameGeneration=2;
  await context.handleClientFrame(new Int16Array(512),1);
});
