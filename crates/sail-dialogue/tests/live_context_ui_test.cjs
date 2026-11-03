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
