'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { segment, Narrator, AudioClock } = require('../web/reader-audio.js');
const { Api } = require('../web/reader.js');
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
function fixture(options = {}) {
  const calls = [], starts = [], pending = [], states = [];
  const tts = { load: async () => {}, speak: text => { calls.push(text); const d = deferred(); pending.push(d); return d.promise; } };
  const clock = { active: null, play(pcm, start, end) { const item = { pcm, start, end }; this.active = item; starts.push(item); return item; }, pause() {}, resume: async () => {}, stop() { this.active = null; } };
  const events = [];
  const n = new Narrator({ tts, clock, onState: s => states.push(s), onSegment: (s, follow) => events.push({s, follow}), ...options });
  n.setText(segment([{ text: 'First. Second. Third. Fourth. Fifth. Sixth.' }], [{block:0,char:7}]));
  return {n, tts, clock, calls, starts, pending, events, states};
}
const pcm = () => ({audio: new Float32Array(20), sampling_rate:24000});
test('segmentation preserves text and source anchors, bounded at measured pages', () => {
  const text = 'A sentence. Another sentence across pages. Last one!';
  const out = segment([{text}], [{block:0,char:18}], 14, 2);
  assert.equal(out.map(s => s.text).join(''), text);
  for (const s of out) { assert.ok(s.text.length <= 14); assert.equal(s.text, text.slice(s.anchor.char, s.end)); assert.equal(s.anchor.section, 2); assert.ok(!(s.anchor.char < 18 && s.end > 18)); }
});
test('segmentation handles UTF-16 surrogate pairs, empty blocks and duplicate boundaries', () => {
  const out = segment([{text:''}, {text:'😀😀 hello!'}], [{block:1,char:1},{block:1,char:1}], 3);
  assert.equal(out.map(s => s.text).join(''), '😀😀 hello!');
  assert.ok(out.every(s => !/^[\uDC00-\uDFFF]|[\uD800-\uDBFF]$/.test(s.text)));
});
test('prefetch is bounded to current plus 1–3 ahead; completion never turns pages', async () => {
  const f = fixture({prefetch:100}); const playing = f.n.play(); await tick();
  assert.equal(f.calls.length, 4); assert.equal(f.events.length, 0);
  for (const d of f.pending) d.resolve(pcm()); await playing;
  assert.equal(f.events.length, 0); assert.equal(f.starts.length, 1);
  f.clock.active.start(); assert.equal(f.events.length, 1);
  assert.equal(f.n.cache.size, 4);
});
test('page follow fires at actual playback start of next-page segment, never on synthesis', async () => {
  const f = fixture(); const p = f.n.play(); await tick(); f.pending.forEach(d => d.resolve(pcm())); await p;
  assert.equal(f.events.length, 0);
  f.clock.active.start(); assert.equal(f.events[0].s.anchor.char, 0);
  f.clock.active.end(); await tick(); assert.equal(f.events.length, 1);
  f.clock.active.start(); assert.equal(f.events[1].s.anchor.char, 7); assert.equal(f.events[1].follow, true);
});
test('pause during synthesis cannot start audio or follow; resume reuses bounded queue', async () => {
  const f = fixture(); const p = f.n.play(); await tick(); f.n.pause(); f.pending.forEach(d => d.resolve(pcm())); await p;
  assert.equal(f.n.state, 'paused'); assert.equal(f.starts.length, 0); assert.equal(f.events.length, 0);
  await f.n.play(); assert.equal(f.starts.length, 1); f.clock.active.start(); assert.equal(f.events.length, 1);
});
test('no automatic page move while paused, including late playback callbacks', async () => {
  const f = fixture(); const p = f.n.play(); await tick(); f.pending.forEach(d => d.resolve(pcm())); await p;
  f.n.pause(); f.clock.active.start(); assert.equal(f.events.length, 0);
  f.clock.active.end(); assert.equal(f.n.state, 'paused'); assert.equal(f.events.length, 0);
});
test('manual navigation disables follow until explicit rejoin; auto off disables turns', async () => {
  const f = fixture(); const p = f.n.play(); await tick(); f.pending.forEach(d => d.resolve(pcm())); await p;
  f.n.manualNavigation(); f.clock.active.start(); assert.equal(f.events[0].follow, false);
  f.n.follow = true; f.n.auto = false; f.clock.active.start(); assert.equal(f.events[1].follow, false);
  f.n.auto = true; f.clock.active.start(); assert.equal(f.events[2].follow, true);
});
test('seek cancels stale synthesis and playback callbacks without advancing new source', async () => {
  const f = fixture(); const first = f.n.play(); await tick(); const old = [...f.pending];
  const second = f.n.seek(2); await tick(); old.forEach(d => d.resolve(pcm())); await first;
  assert.equal(f.starts.length, 0); assert.equal(f.events.length, 0);
  f.pending.slice(old.length).forEach(d => d.resolve(pcm())); await second;
  f.clock.active.start(); assert.equal(f.events[0].s.anchor.char, 15);
  const obsolete = f.clock.active; f.n.cancel(); obsolete.start(); obsolete.end(); assert.equal(f.n.state, 'canceled'); assert.equal(f.events.length, 1);
});
test('voice change while paused drops old PCM and does not restart playback', async () => {
  const f = fixture(); const p = f.n.play(); await tick(); f.n.pause(); await f.n.setVoice('bf_emma');
  f.pending.forEach(d => d.resolve(pcm())); await p;
  assert.equal(f.n.state, 'paused'); assert.equal(f.n.voice, 'bf_emma'); assert.equal(f.events.length, 0); assert.equal(f.n.cache.size, 0);
});
test('cancel while loading prevents synthesis and error callbacks', async () => {
  const f = fixture(), load = deferred(); f.tts.load = () => load.promise;
  const p = f.n.play(); f.n.cancel(); load.reject(new Error('late')); await p;
  assert.equal(f.n.state, 'canceled'); assert.equal(f.calls.length, 0);
});
test('rejected stale synthesis cannot set error in a newer generation', async () => {
  const f = fixture(); const p = f.n.play(); await tick(); const old = [...f.pending]; f.n.cancel();
  old.forEach(d => d.reject(new Error('failed old generation'))); await p; assert.equal(f.n.state, 'canceled');
});
test('AudioClock starts from the Web Audio clock and does not restart stopped source', async () => {
  const gate = deferred(), sources = [], notices = [];
  const context = { state:'suspended', currentTime:42, destination:{}, createBuffer:()=>({copyToChannel(){}}), createBufferSource:()=>{ const s = {connect(){}, start:t=>notices.push(t), stop(){}}; sources.push(s); return s; }, resume:()=>gate.promise, suspend:async()=>{} };
  const clock = new AudioClock(context); clock.play(pcm(), ()=>notices.push('started'), ()=>{});
  await tick(); assert.equal(notices.length,0); clock.pause(); context.state='running'; gate.resolve(); await tick(); assert.equal(notices.length,0);
  await clock.resume(); assert.deepEqual(notices,[42,'started']); clock.stop(); assert.equal(sources[0].onended,null);
});
test('API uses owner authorization, raw imports and JSON CAS positions', async () => {
  const calls = [], storage = {getItem:()=> 't'.repeat(32), setItem(){}};
  const api = new Api(async (url, options) => { calls.push({url,...options}); return {ok:true,status:201,json:async()=>({book:{id:'one'}})}; }, storage);
  const bytes = new Uint8Array([1,2]); await api.request('/import',{method:'POST',body:bytes,filename:'book.epub'});
  assert.equal(calls[0].headers.Authorization,`Bearer ${'t'.repeat(32)}`); assert.equal(calls[0].headers['X-Filename'],'book.epub'); assert.equal(calls[0].body,bytes);
  await api.request('/books/one/position',{method:'PUT',body:{expected_revision:3,anchor:{section:1,block:2,char:3}}});
  assert.equal(JSON.parse(calls[1].body).expected_revision,3); assert.equal(calls[1].headers['Content-Type'],'application/json');
});
test('API exposes stale revision conflict instead of silently overwriting it', async () => {
  const position = {revision:4,anchor:{section:1,block:0,char:0}};
  const api = new Api(async()=>({ok:false,status:409,json:async()=>({error:'Opened elsewhere',position})}),{getItem:()=> 't'.repeat(32)});
  await assert.rejects(api.request('/books/one/position',{method:'PUT',body:{expected_revision:3}}),error=>error.status===409 && error.position===position);
});
test('API refreshes rejected owner token once and never sends text to other endpoints', async () => {
  const calls=[]; const api=new Api(async(url, options)=>{
    calls.push({url,options}); if(url==='/api/guardian/token') return {ok:true,json:async()=>({token:'new'})};
    if(calls.length===1) return {ok:false,status:401}; return {ok:true,status:200,json:async()=>({books:[]})};
  },{getItem:()=> 't'.repeat(32),setItem(){}});
  await api.request('/library'); assert.equal(calls.length,3); assert.equal(calls[2].options.headers.Authorization,'Bearer new');
});
function speechFixture() {
  const workers=[],blobs=[];
  class FakeWorker {
    constructor(url){ this.url=url;workers.push(this); queueMicrotask(()=>this.onmessage({data:{ready:true,voices:['af_heart']}})); }
    postMessage(data){ queueMicrotask(()=>this.onmessage({data:{id:data.id,ok:true,result:data.op==='load'?{voices:['af_heart'],device:'wasm'}:pcm()}})); }
    terminate(){this.stopped=true;}
  }
  class FakeBlob{constructor(parts){blobs.push(parts.join(''));}}
  const sandbox={Worker:FakeWorker,WebAssembly:{},Blob:FakeBlob,URL:{createObjectURL:()=> 'blob:test',revokeObjectURL(){}},location:{origin:'https://sail.test'},fetch:async()=>({ok:true}),localStorage:{getItem(){return null;}},setTimeout,clearTimeout,setInterval,clearInterval,performance,Float32Array,Int16Array};
  vm.createContext(sandbox);vm.runInContext(fs.readFileSync(path.join(__dirname,'../web/client-speech.js'),'utf8')+'\nthis.speech=SailClientSpeech;',sandbox);
  return {sandbox,workers,blobs,speech:sandbox.speech};
}
test('TTS-only initialization loads one Kokoro worker, no VAD/Whisper, reuses it', async()=>{
  const f=speechFixture(); const [a,b]=await Promise.all([f.speech.loadTts(),f.speech.loadTts()]); assert.equal(a.device,'wasm');assert.equal(b.device,'wasm');assert.equal(f.workers.length,1);
  await f.speech.loadTts();assert.equal(f.workers.length,1);assert.equal(f.workers[0].url,'/client-speech-worker.js?role=tts');assert.equal(f.blobs.length,0);
  assert.equal(f.speech.engine.status,'off');await f.speech.speak('source text','af_heart');
  f.speech.stop();assert.equal(f.workers[0].stopped,true);
});
test('TTS-only load reuses a Dialogue resident voice without another worker',async()=>{
  const f=speechFixture(); f.speech.engine.speechWorker={call:async()=>pcm()}; f.speech.engine.voices=['af_heart'];f.speech.engine.status='ready';
  assert.equal((await f.speech.loadTts()).device,'wasm');assert.equal(f.workers.length,0);
});
test('stopped TTS loading cannot install a late worker',async()=>{
  const f=speechFixture(),gate=deferred();f.sandbox.fetch=()=>gate.promise;
  const p=f.speech.loadTts();f.speech.stop();gate.resolve({ok:true});await assert.rejects(p,/canceled/);assert.equal(f.workers.length,0);
});
test('untrusted book strings only reach textContent; page assets are embedded',()=>{
  const js=fs.readFileSync(path.join(__dirname,'../web/reader.js'),'utf8'),html=fs.readFileSync(path.join(__dirname,'../web/reader.html'),'utf8'),rust=fs.readFileSync(path.join(__dirname,'../src/ui_assets.rs'),'utf8');
  assert.ok(!/innerHTML|insertAdjacentHTML|reason_turn|\/api\/chat|MindService/.test(js));
  for(const name of ['reader.html','reader.css','reader.js','reader-audio.js']) assert.ok(rust.includes(`web/${name}`));
  assert.ok(html.includes('aria-pressed="true"'));assert.ok(html.includes('role="status"'));assert.ok(html.includes('Follow narration'));
});
test('pause then resume before loading resolves cannot play a segment twice', async()=>{
  const f=fixture(),gate=deferred();f.tts.load=()=>gate.promise;
  const first=f.n.play();f.n.pause();const second=f.n.play();gate.resolve();await tick();
  f.pending.forEach(d=>d.resolve(pcm()));await Promise.all([first,second]);assert.equal(f.starts.length,1);
});
test('pause then resume before PCM resolves cannot enqueue duplicate playback',async()=>{
  const f=fixture();const first=f.n.play();await tick();f.n.pause();const second=f.n.play();await tick();
  f.pending.forEach(d=>d.resolve(pcm()));await Promise.all([first,second]);assert.equal(f.starts.length,1);
});

test('shared worker tts role loads only local q8 WASM Kokoro and preserves Dialogue speech role', async () => {
  const source = fs.readFileSync(path.join(__dirname, '../web/client-speech-worker.js'), 'utf8');
  for (const role of ['tts', 'speech']) {
    const loads = [], messages = [];
    const sandbox = {
      URL, performance, Float32Array,
      navigator: {},
      self: {
        location: { href: `https://sail.test/client-speech-worker.js?role=${role}`, origin: 'https://sail.test' },
        fetch: async () => ({ json: async () => ({ files: {} }) }),
        postMessage: message => messages.push(message),
      },
      transformers: {
        env: { backends: { onnx: { wasm: {} } } },
        pipeline: async (...args) => { loads.push(['whisper', ...args]); return {}; },
      },
      kokoro: {
        env: {},
        KokoroTTS: { from_pretrained: async (model, options) => {
          loads.push(['kokoro', model, options]);
          return { voices: { af_heart: {} }, generate: async () => pcm() };
        } },
      },
    };
    vm.createContext(sandbox);
    const executable = source
      .replace('await import(`${ASSETS}lib/transformers.min.js`)', 'globalThis.transformers')
      .replace('await import(`${ASSETS}lib/kokoro.web.js`)', 'globalThis.kokoro');
    await vm.runInContext(`(async () => { ${executable}\nthis.files = filesFor(ROLE, false); })()`, sandbox);
    sandbox.self.onmessage({ data: { id: 1, op: 'load' } });
    await tick();
    const result = messages.find(m => m.id === 1);
    assert.equal(result.ok, true);
    assert.equal(result.result.device, 'wasm');
    assert.equal(loads.filter(l => l[0] === 'kokoro').length, 1);
    const options = loads.find(l => l[0] === 'kokoro')[2];
    assert.equal(options.dtype, 'q8');
    assert.equal(options.device, 'wasm');
    assert.equal(loads.some(l => l[0] === 'whisper'), role === 'speech');
    assert.equal(sandbox.files.some(f => f.includes('whisper')), role === 'speech');
    if (role === 'tts') assert.ok(sandbox.files.every(f => f.includes('Kokoro')));
    sandbox.self.onmessage({ data: { id: 2, op: 'speak', text: 'Text', voice: 'af_heart' } });
    await tick();
    assert.equal(messages.find(m => m.id === 2).ok, true);
  }
});

test('continuous narration prefetches at final playback start and follows next section only on playback', async () => {
  const gate = deferred(), requests = [];
  const f = fixture({ loadMore: (last, generation) => {
    requests.push({ last, generation });
    return last.anchor.section === 0 ? gate.promise : [];
  } });
  f.n.setText(segment([{ text: 'End.' }], [], 320, 0));
  const play = f.n.play(); await tick();
  f.pending[0].resolve(pcm()); await play;
  assert.equal(requests.length, 0, 'synthesis does not fetch the next section');
  f.starts[0].start(); await tick();
  assert.equal(requests.length, 1);
  gate.resolve(segment([{ text: 'Next. More. Last. Again.' }], [{block:0,char:6}], 320, 1));
  await tick();
  assert.equal(f.calls.length, 3, 'current plus bounded two ahead across the boundary');
  f.pending.slice(1).forEach(d => d.resolve(pcm()));
  await tick();
  assert.deepEqual(f.events.map(e => e.s.anchor.section), [0]);
  f.starts[0].end(); await tick();
  assert.deepEqual(f.events.map(e => e.s.anchor.section), [0], 'scheduling cannot follow');
  f.starts[1].start();
  assert.deepEqual(f.events.map(e => e.s.anchor.section), [0, 1]);
  assert.ok(!f.states.includes('completed'), 'no completed state between sections');
});

test('section prefetch arriving after cancel is dropped; paused boundary resumes once', async () => {
  for (const cancel of [true, false]) {
    const gate = deferred();
    const f = fixture({ loadMore: last => last.anchor.section === 0 ? gate.promise : [] });
    f.n.setText(segment([{text:'End.'}], [], 320, 0));
    const play = f.n.play(); await tick(); f.pending[0].resolve(pcm()); await play;
    f.starts[0].start(); await tick(); f.starts[0].end(); await tick();
    if (cancel) f.n.cancel(); else f.n.pause();
    gate.resolve(segment([{text:'Next.'}], [], 320, 1)); await tick();
    assert.equal(f.starts.length, 1);
    if (cancel) {
      assert.equal(f.n.segments.length, 1);
      assert.equal(f.n.cache.size, 0);
      continue;
    }
    const resume = f.n.play(); await tick();
    f.pending[1].resolve(pcm()); await resume;
    assert.equal(f.starts.length, 2);
    f.n.manualNavigation(); f.starts[1].start();
    assert.equal(f.events[1].follow, false);
    await tick(); f.starts[1].end(); await tick();
    assert.equal(f.n.state, 'completed', 'end of book completes');
  }
});
test('last segment playback prefetches next section and continues without completed between sections', async () => {
  const section1 = segment([{text:'One.'}], [], 320, 0);
  const section2 = segment([{text:'Two. Three.'}], [], 320, 1);
  const states = [], observed = [], loads = [];
  const clock = {active:null,play(pcm,start,end){this.active={start,end};return this.active;},stop(){this.active=null;},pause(){},resume:async()=>{}};
  const narrator = new Narrator({clock,tts:{load:async()=>{},speak:async()=>pcm()},onState:s=>states.push(s),onSegment:s=>observed.push(s.anchor.section),loadMore:async(last)=>{loads.push(last.anchor.section);return last.anchor.section===0?section2:[];}});
  narrator.setText(section1);await narrator.play();assert.deepEqual(loads,[]);
  clock.active.start();await tick();assert.deepEqual(loads,[0]);assert.deepEqual(observed,[0]);
  assert.ok(narrator.cache.size<=narrator.ahead+1);
  clock.active.end();await tick();assert.equal(narrator.state,'buffering');assert.ok(!states.includes('completed'));
  clock.active.start();assert.deepEqual(observed,[0,1]);clock.active.end();await tick();clock.active.start();await tick();clock.active.end();await tick();
  assert.equal(narrator.state,'completed');assert.deepEqual(loads,[0,1]);assert.equal(narrator.cache.size,0);
});
test('late next-section response after cancel never appends text or begins synthesis',async()=>{
  const f=fixture(),gate=deferred();f.n.setText(segment([{text:'Only.'}]));f.n.loadMore=()=>gate.promise;
  const p=f.n.play();await tick();f.pending[0].resolve(pcm());await p;f.clock.active.start();await tick();
  f.n.cancel();gate.resolve(segment([{text:'Stale section.'}],[],320,1));await tick();
  assert.equal(f.n.segments.length,1);assert.equal(f.n.state,'canceled');assert.equal(f.calls.length,1);
});
test('next-section completion while paused cannot start playback or turn pages',async()=>{
  const f=fixture(),gate=deferred();f.n.setText(segment([{text:'Only.'}]));f.n.loadMore=()=>gate.promise;
  const p=f.n.play();await tick();f.pending[0].resolve(pcm());await p;f.clock.active.start();await tick();
  f.n.pause();gate.resolve(segment([{text:'Next.'}],[],320,1));await tick();
  assert.equal(f.events.length,1);assert.equal(f.starts.length,1);assert.equal(f.n.state,'paused');
});
test('tts worker role loads only Kokoro through the shared local asset mapping',()=>{
  const worker=fs.readFileSync(path.join(__dirname,'../web/client-speech-worker.js'),'utf8');
  assert.ok(worker.includes("if (role === 'tts') return kokoro") || /if \(role === 'tts'\)\s+return kokoro/.test(worker));
  assert.ok(worker.includes("ROLE === 'tts' ? loadTts()"));
  assert.ok(!fs.readFileSync(path.join(__dirname,'../web/client-speech.js'),'utf8').includes('new Blob'));
});

test('full Dialogue load retires the TTS-only worker before creating its speech chain', async () => {
  const f = speechFixture();
  await f.speech.loadTts();
  const ttsWorker = f.workers[0];
  await f.speech.load();
  assert.equal(ttsWorker.stopped, true);
  assert.deepEqual(f.workers.map(w => w.url), [
    '/client-speech-worker.js?role=tts',
    '/client-speech-worker.js?role=vad',
    '/client-speech-worker.js?role=speech',
  ]);
});

test('reader preserves verse line breaks, pre spacing, TOC block anchors and Finished state', () => {
  const css = fs.readFileSync(path.join(__dirname, '../web/reader.css'), 'utf8');
  const js = fs.readFileSync(path.join(__dirname, '../web/reader.js'), 'utf8');
  assert.match(css, /#pages\s*>\s*\*\s*\{[^}]*white-space:\s*pre-line/);
  assert.match(css, /#pages\s+pre\s*\{[^}]*white-space:\s*pre-wrap/);
  assert.match(js, /completed:\s*'Finished'/);
  assert.match(js, /block:\s*item.block\s*\|\|\s*0/);
});
test('a failed TTS worker rejects subsequent synthesis and can be reloaded', async () => {
  const f = speechFixture();
  await f.speech.loadTts();
  const first = f.workers[0];
  first.onerror({preventDefault(){},message:'worker crashed'});
  await assert.rejects(f.speech.speak('literal text','af_heart'), /worker crashed/);
  await f.speech.loadTts();
  assert.equal(f.workers.length, 2);
  assert.equal(first.stopped, true);
});

test('footer finds the nearest nested TOC anchor by section and block, with section fallback', () => {
  const { chapterLabel } = require('../web/reader.js');
  const toc = [
    { title: 'Later', section: 1, block: 0 },
    { title: 'Part', section: 0, block: 4, children: [
      { title: 'Chapter II', section: 0, block: 20 },
      { title: 'Chapter I', section: 0, block: 5 }
    ] }
  ];
  const label = (section, block, char = 0) => chapterLabel(toc, {section, block, char}, 'Section title');
  assert.equal(label(0, 0), 'Section title');
  assert.equal(label(0, 4), 'Part');
  assert.equal(label(0, 5), 'Chapter I');
  assert.equal(label(0, 19, 100), 'Chapter I');
  assert.equal(label(0, 20), 'Chapter II');
  assert.equal(label(1, 0), 'Later');
  assert.equal(chapterLabel([], {section: 0, block: 0, char: 0}, 'Fallback'), 'Fallback');
});

test('slow-device note uses measured synthesis versus PCM duration and reports once', async () => {
  let time = 0, notes = 0;
  const f = fixture({now: () => time, onSlow: () => notes++});
  const p = f.n.play(); await tick();
  assert.equal(notes, 0);
  assert.equal(f.n.hasPlayed, false);
  time = 500;
  f.pending[0].resolve({audio: new Float32Array(1000), sampling_rate: 1000});
  await p;
  assert.equal(notes, 0, 'synthesis faster than audio does not warn');
  f.clock.active.start();
  assert.equal(f.n.hasPlayed, true);
  f.clock.active.end(); await tick();
  assert.equal(f.n.state, 'buffering');
  time = 1500;
  f.pending.slice(1).forEach(d => d.resolve({audio: new Float32Array(1000), sampling_rate: 1000}));
  await tick();
  assert.equal(notes, 1);
  f.clock.active.start(); f.clock.active.end(); await tick();
  time = 4000;
  f.pending.forEach(d => d.resolve(pcm())); await tick();
  assert.equal(notes, 1);
  const source = fs.readFileSync(path.join(__dirname, '../web/reader.js'), 'utf8');
  assert.match(source, /hasPlayed \? 'Waiting for the voice…' : 'Preparing audio…'/);
});

test('canceled synthesis cannot show a stale slow-device note', async () => {
  let time = 0, notes = 0;
  const f = fixture({now: () => time, onSlow: () => notes++});
  const p = f.n.play(); await tick(); f.n.cancel(); time = 10000;
  f.pending.forEach(d => d.resolve(pcm())); await p;
  assert.equal(notes, 0);
});

const { HostEngine, VoiceRouter } = require('../web/reader-audio.js');
const hostPcm = () => ({ ok: true, status: 200,
  headers: { get: name => name === 'X-Sample-Rate' ? '16000' : '12' },
  arrayBuffer: async () => new Uint8Array([0, 128, 0, 0, 255, 127]).buffer });
function hostFixture(fetcher, extra = {}) {
  return new HostEngine({ fetcher, getToken: async () => 'owner-token', ...extra });
}
test('host decodes signed little-endian PCM and real sample rate with authenticated POST', async () => {
  const host = hostFixture(async (url, request) => {
    assert.equal(url, '/api/reader/speech');
    assert.equal(request.headers.Authorization, 'Bearer owner-token');
    assert.deepEqual(JSON.parse(request.body), { text: 'Literal text.', voice: 'af_heart', speed: 1.25 });
    return hostPcm();
  }, { speed: 1.25 });
  const result = await host.speak('Literal text.', 'af_heart');
  assert.deepEqual([...result.audio], [-1, 0, 32767 / 32768]);
  assert.equal(result.sampling_rate, 16000);
  assert.equal(result.synthesisMs, 12);
  assert.equal(result.backend, 'host');
});
test('host serializes requests including 429 retry', async () => {
  const gate = deferred(); let count = 0, delays = 0;
  const host = hostFixture(async () => {
    count++;
    if (count === 1) return { status: 429 };
    if (count === 2) await gate.promise;
    return hostPcm();
  }, { delay: async () => { delays++; } });
  const a = host.speak('One.', 'af_heart'), b = host.speak('Two.', 'af_heart');
  await tick(); assert.equal(count, 2); assert.equal(delays, 1);
  gate.resolve(); await Promise.all([a, b]); assert.equal(count, 3);
});
test('generation cancellation aborts active host fetch and discards queued requests', async () => {
  let signal, count = 0;
  const host = hostFixture(async (_, request) => {
    count++; signal = request.signal;
    return new Promise((resolve, reject) => signal.addEventListener('abort', () => reject(Error('abort'))));
  });
  const a = host.speak('One.', 'af_heart'), b = host.speak('Two.', 'af_heart');
  const settled = Promise.allSettled([a, b]);
  await tick(); host.cancel();
  assert.equal(signal.aborted, true);
  assert.ok((await settled).every(r => r.status === 'rejected'));
  assert.equal(count, 1);
});
function routerFixture(extra = {}) {
  return new VoiceRouter({ host: { cancel() {}, speak: async () => pcm() },
    device: { load: async () => {}, speak: async () => pcm() },
    capabilities: { available: true }, ...extra });
}
test('host is default; explicit device preference and unavailable host are respected', () => {
  assert.equal(routerFixture().backend, 'host');
  assert.equal(routerFixture({ preference: 'device' }).backend, 'device');
  assert.equal(routerFixture({ capabilities: { available: false } }).backend, 'device');
});
test('two consecutive slow device segments request switch at next unspoken sentence', async () => {
  let time = 0;
  const router = routerFixture({ preference: 'device', now: () => time,
    device: { load: async () => {}, speak: async () => { time += 2000; return pcm(); } } });
  await router.speak('One.'); assert.ok(!router.switchPending);
  await router.speak('Two.'); assert.equal(router.switchPending, true);
  const f = fixture({ tts: router });
  f.n.index = 2;
  await f.n.play();
  assert.equal(router.backend, 'host');
  assert.equal(f.n.index, 2);
  assert.equal(f.events.length, 0);
});
test('503 and network failures fall back to loadable device; device load failures surface', async () => {
  for (const status of [503, undefined]) {
    let loads = 0;
    const router = routerFixture({ host: { cancel() {}, speak: async () => { throw Object.assign(Error(), { status }); } },
      device: { load: async () => { loads++; }, speak: async () => pcm() } });
    await router.speak('One.'); assert.equal(router.backend, 'device'); assert.equal(loads, 1);
  }
  const router = routerFixture({ host: { cancel() {}, speak: async () => { throw Error(); } },
    device: { load: async () => { throw Error('unavailable'); } } });
  await assert.rejects(router.speak('One.'), /unavailable/);
});
test('speed restart changes generation at the current sentence and preserves pause', () => {
  const router = routerFixture(); const f = fixture({ tts: router });
  f.n.index = 2; f.n.setState('paused');
  const generation = f.n.generation;
  f.n.setSpeed(1.5);
  assert.equal(f.n.generation, generation + 1);
  assert.equal(router.host.speed, 1.5);
  assert.equal(f.n.index, 2); assert.equal(f.n.state, 'paused');
  const source = fs.readFileSync(path.join(__dirname, '../web/reader.js'), 'utf8');
  assert.match(source, /persist\('speed', String\(speed\)\)/);
  assert.match(source, /\$\('speed-control'\)\.hidden = !host/);
});

test('host failure prevents repeated automatic host/device cycling in the same session', async () => {
  const router = routerFixture({ host: { cancel() {}, speak: async () => { throw Error('offline'); } } });
  await router.speak('One.');
  assert.equal(router.capabilities.available, false);
  assert.equal(router.backend, 'device');
  await assert.rejects(router.select('host'), /unavailable/);
});

test('host selection supplies endpoint default when the old device voice is unsupported', async () => {
  let selected;
  const router = routerFixture({ capabilities: {
    available: true, voices: ['bf_emma'], default_voice: 'bf_emma'
  }, host: { cancel() {}, speak: async (_, voice) => { selected = voice; return pcm(); } } });
  await router.speak('One.', 'unknown-device-voice');
  assert.equal(selected, 'bf_emma');
});
