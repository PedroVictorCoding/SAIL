// Run with node --test crates/sail-dialogue/tests/voice_learning_ui_test.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(`${__dirname}/../web/voice.js`, 'utf8');
function ui(fetch) {
  const elements = Object.fromEntries(['learningBtn','learningStatus','learningIndicator'].map(id => [id, {
    setAttribute(k,v) { this[k] = v; }, addEventListener(_, callback) { this.click = callback; }
  }]));
  const context = vm.createContext({document:{getElementById:id=>elements[id]}, fetch,
    getOrFetchOwnerToken:async()=>'owner', showError:message=>{context.error=message;},
    learningSession:'session', learningEnabled:false, learningBusy:false});
  vm.runInContext(source.slice(source.indexOf('function renderLearning()')), context);
  context.renderLearning();
  return {context, elements, button:elements.learningBtn};
}
test('owner request enables learning only after backend acknowledgment', async () => {
  let resolve;
  const {context, elements, button} = ui((url, options) => {
    assert.equal(url, '/api/memory/learning');
    assert.equal(options.headers.Authorization, 'Bearer owner');
    assert.deepEqual(JSON.parse(options.body), {session:'session',enabled:true});
    return new Promise(r=>{resolve=r;});
  });
  const pending = button.click();
  await new Promise(setImmediate);
  assert.equal(context.learningEnabled, false);
  assert.equal(button.disabled, true);
  resolve({ok:true,json:async()=>({enabled:true})});
  await pending;
  assert.equal(button['aria-pressed'], 'true');
  assert.equal(elements.learningIndicator.hidden, false);
});
test('failed change displays unknown state and the next action disables learning', async () => {
  let fail = true;
  const {context, elements, button} = ui(async (_, options) => {
    if(fail) throw Error('offline');
    assert.equal(JSON.parse(options.body).enabled, false);
    return {ok:true,json:async()=>({enabled:false})};
  });
  await button.click();
  assert.equal(context.learningEnabled, null);
  assert.match(elements.learningStatus.textContent, /State unavailable/);
  assert.equal(elements.learningIndicator.hidden, false);
  fail = false;
  await button.click();
  assert.equal(context.learningEnabled, false);
});
test('a delayed response cannot enable a replacement connection', async () => {
  let resolve;
  const {context, button} = ui(()=>new Promise(r=>{resolve=r;}));
  const pending = button.click();
  await new Promise(setImmediate);
  context.learningSession = 'replacement';
  resolve({ok:true,json:async()=>({enabled:true})});
  await pending;
  assert.equal(context.learningEnabled, false);
  context.learningSession = null;
  context.renderLearning();
  assert.equal(button.disabled, true);
});
