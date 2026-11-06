// SAIL's speech models running on this device: Silero VAD (role "vad") or Whisper base and
// Kokoro (role "speech"), or Kokoro alone (role "tts"). The same models as the host workers,
// loaded from SAIL's own origin (/voice-assets, installed by scripts/voice/download_web.py). Nothing is fetched
// from the internet: Hugging Face and CDN URLs inside the libraries are mapped to local
// files, and any other request to those hosts is refused.
const ROLE = new URL(self.location.href).searchParams.get('role');
const ASSETS = new URL('/voice-assets/', self.location.origin).href;
const TRANSFORMERS_CDN = 'https://cdn.jsdelivr.net/npm/@huggingface/transformers@3.5.1/dist/';

function localUrl(url) {
  const hf = /^https:\/\/huggingface\.co\/([^/]+\/[^/]+)\/resolve\/[^/]+\/(.+)$/.exec(url);
  if (hf) return `${ASSETS}hf/${hf[1]}/${hf[2]}`;
  if (url.startsWith(TRANSFORMERS_CDN)) return `${ASSETS}lib/${url.slice(TRANSFORMERS_CDN.length)}`;
  if (/^https?:\/\/(huggingface\.co|cdn\.jsdelivr\.net)\//.test(url)) return null;
  return url;
}
const realFetch = self.fetch.bind(self);
self.fetch = (input, init) => {
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
  const local = localUrl(url);
  if (local === null) return Promise.reject(new TypeError(`Refused off-device request: ${url}`));
  return realFetch(local === url ? input : local, init);
};

const { env, pipeline, AutoModel, Tensor } = await import(`${ASSETS}lib/transformers.min.js`);
env.allowLocalModels = false;
env.remoteHost = `${ASSETS}hf/`;
env.remotePathTemplate = '{model}/';
env.backends.onnx.wasm.wasmPaths = `${ASSETS}lib/`;

async function webgpuAvailable() {
  try { return Boolean(await navigator.gpu?.requestAdapter()); } catch { return false; }
}

// Download progress for the page's loading bar: bytes per file as transformers.js reports
// them (it reports cached files too, read from the browser cache).
// A finished file reports its full size from the manifest ('done' events carry no bytes).
let manifestSizes = {};
function reportProgress(info) {
  if (info.status !== 'progress' && info.status !== 'done') return;
  const file = `${info.name}/${info.file}`;
  if (info.status === 'done') {
    const bytes = manifestSizes[`hf/${file}`] ?? 0;
    self.postMessage({ progress: { role: ROLE, file, loaded: bytes, total: bytes, done: true } });
  } else {
    self.postMessage({ progress: { role: ROLE, file, loaded: info.loaded ?? 0, total: info.total ?? 0, done: false } });
  }
}

let vad = null, vadState = null, vadContext = null;
let asr = null, tts = null, device = 'wasm';

// Exactly the files each load below fetches (keep in step with its dtype choices), so the
// page's loading bar can measure against the whole download from the start.
const WHISPER = 'hf/onnx-community/whisper-base/', KOKORO = 'hf/onnx-community/Kokoro-82M-v1.0-ONNX/';
function filesFor(role, gpu) {
  if (role === 'vad') return ['hf/onnx-community/silero-vad/onnx/model.onnx'];
  const kokoro = ['config.json', 'tokenizer.json', 'tokenizer_config.json', 'onnx/model_quantized.onnx']
    .map(f => KOKORO + f);
  if (role === 'tts') return kokoro;
  return [
    ...['config.json', 'generation_config.json', 'preprocessor_config.json', 'tokenizer.json', 'tokenizer_config.json'].map(f => WHISPER + f),
    ...(gpu ? ['onnx/encoder_model.onnx', 'onnx/decoder_model_merged_q4.onnx'] : ['onnx/encoder_model_quantized.onnx', 'onnx/decoder_model_merged_quantized.onnx']).map(f => WHISPER + f),
    // Kokoro TTS uses quantized WASM on both GPU and CPU devices to avoid the WebGPU ConvTranspose vocoder distortion.
    ...kokoro,
  ];
}
async function reportExpected(gpu) {
  try {
    const manifest = await (await realFetch(`${ASSETS}manifest.json`, { cache: 'no-cache' })).json();
    manifestSizes = Object.fromEntries(Object.entries(manifest.files).map(([f, v]) => [f, v.bytes]));
    const sizes = filesFor(ROLE, gpu).map(f => manifest.files[f]?.bytes);
    if (sizes.every(Number.isFinite)) self.postMessage({ progress: { role: ROLE, expected: sizes.reduce((a, b) => a + b, 0) } });
  } catch { /* older manifest without sizes: the bar uses the sizes it has seen */ }
}

async function loadVad() {
  await reportExpected(false);
  // Silero v5 at 16 kHz: 512-sample frames with 64 samples of context, as the host runs it.
  const model = await AutoModel.from_pretrained('onnx-community/silero-vad', {
    config: { model_type: 'custom' }, dtype: 'fp32', device: 'wasm', progress_callback: reportProgress,
  });
  vad = model; resetVad();
  return { device: 'wasm' };
}
function resetVad() {
  vadState = new Tensor('float32', new Float32Array(2 * 1 * 128), [2, 1, 128]);
  vadContext = new Float32Array(64);
}
async function vadFrame(samples) {
  const input = new Float32Array(576);
  input.set(vadContext); input.set(samples, 64);
  const started = performance.now();
  const { output, stateN } = await vad({
    input: new Tensor('float32', input, [1, 576]),
    sr: new Tensor('int64', BigInt64Array.from([16000n]), []),
    state: vadState,
  });
  vadState = stateN; vadContext = input.slice(-64);
  return { probability: output.data[0], inference_ms: performance.now() - started };
}

async function loadSpeech() {
  device = (await webgpuAvailable()) ? 'webgpu' : 'wasm';
  self.postMessage({ progress: { role: ROLE, device } });
  const gpu = device === 'webgpu';
  await reportExpected(gpu);
  asr = await pipeline('automatic-speech-recognition', 'onnx-community/whisper-base', {
    device, progress_callback: reportProgress,
    dtype: gpu ? { encoder_model: 'fp32', decoder_model_merged: 'q4' } : { encoder_model: 'q8', decoder_model_merged: 'q8' },
  });
  return loadTts();
}

async function loadTts() {
  if (ROLE === 'tts') {
    device = 'wasm';
    self.postMessage({ progress: { role: ROLE, device } });
    await reportExpected(false);
  }
  const { KokoroTTS, env: kokoroEnv } = await import(`${ASSETS}lib/kokoro.web.js`);
  kokoroEnv.wasmPaths = `${ASSETS}lib/`;
  // Kokoro TTS runs on WASM with q8 quantization even when WebGPU is available for Whisper:
  // ONNX Runtime Web's WebGPU ConvTranspose kernel corrupts vocoder upsampling into harsh,
  // robotic, metallic noise, while WASM CPU SIMD produces pristine, natural human speech.
  tts = await KokoroTTS.from_pretrained('onnx-community/Kokoro-82M-v1.0-ONNX', {
    device: 'wasm',
    dtype: 'q8',
    progress_callback: reportProgress,
  });
  return { device, voices: Object.keys(tts.voices) };
}

// Whisper's own language identification, as the host's faster-whisper does it: one decoder
// step from the start token over the first 30 s, then the most likely language token.
// (transformers.js would otherwise assume English.)
async function detectLanguage(samples) {
  const { model, processor } = asr;
  const { input_features } = await processor(samples.subarray(0, 16000 * 30));
  const config = model.generation_config;
  const start = new Tensor('int64', BigInt64Array.from([BigInt(config.decoder_start_token_id)]), [1, 1]);
  const { logits } = await model({ input_features, decoder_input_ids: start });
  const scores = logits.data, offset = scores.length - logits.dims.at(-1);
  let best = null, bestScore = -Infinity;
  for (const [token, id] of Object.entries(config.lang_to_id)) {
    const score = Number(scores[offset + id]);
    if (score > bestScore) { bestScore = score; best = token.slice(2, -2); }
  }
  return best;
}

async function transcribe(samples) {
  const started = performance.now();
  const language = await detectLanguage(samples);
  const result = await asr(samples, { task: 'transcribe', language, chunk_length_s: 30, stride_length_s: 5 });
  return { text: (result.text || '').trim(), language, inference_ms: performance.now() - started };
}

async function speak(text, voice) {
  const started = performance.now();
  const audio = await tts.generate(text, { voice });
  return { audio: audio.audio, sampling_rate: audio.sampling_rate, inference_ms: performance.now() - started };
}

const handlers = {
  load: () => (ROLE === 'vad' ? loadVad() : ROLE === 'tts' ? loadTts() : loadSpeech()),
  vad: ({ samples }) => vadFrame(samples),
  reset: () => { resetVad(); return {}; },
  transcribe: ({ samples }) => transcribe(samples),
  speak: ({ text, voice }) => speak(text, voice),
};

async function handle({ id, op, ...data }) {
  try {
    const result = await handlers[op](data);
    const transfer = result?.audio ? [result.audio.buffer] : [];
    self.postMessage({ id, ok: true, result }, transfer);
  } catch (error) {
    self.postMessage({ id, ok: false, error: String(error?.message || error) });
  }
}
// One request at a time, in arrival order: Silero's recurrent state must see frames in
// sequence, and Whisper/Kokoro calls must not interleave.
let queue = Promise.resolve();
self.onmessage = ({ data }) => { queue = queue.then(() => handle(data)); };
self.postMessage({ ready: true });
