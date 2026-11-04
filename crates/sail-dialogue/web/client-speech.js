'use strict';
// Runs SAIL's speech chain (Silero VAD, Whisper base, Kokoro) in this browser when the SAIL
// host has the browser assets installed, so the host's CPU stays with the language model.
// The host workers remain the fallback: anything this device cannot run, or that fails,
// is reported to the server and handled there.
const SailClientSpeech = (() => {
  const PREF_KEY = 'sail.speechPlacement';
  // On-device STT or TTS is used only if it measured at least twice as fast as real time
  // here; slower devices would make turns slower than the host does. Advanced override:
  // localStorage 'sail.speechMaxRealTimeFactor' (the browser test raises it on slow CI).
  const maxRealTimeFactor = () => Number(localStorage.getItem('sail.speechMaxRealTimeFactor')) || 0.5;
  const WARM_UP = 'Hello, I am ready to help you today.';

  class EngineWorker {
    constructor(role, onProgress = () => {}) {
      this.worker = new Worker(`/client-speech-worker.js?role=${role}`, { type: 'module' });
      this.nextId = 1; this.pending = new Map();
      this.ready = new Promise((resolve, reject) => {
        this.worker.onmessage = ({ data }) => {
          if (data.ready) { resolve(); return; }
          if (data.progress) { onProgress(data.progress); return; }
          const call = this.pending.get(data.id); if (!call) return;
          this.pending.delete(data.id);
          if (data.ok) call.resolve(data.result); else call.reject(new Error(data.error));
        };
        this.worker.onerror = event => {
          event.preventDefault();
          const error = new Error(event.message || `Speech ${role} worker failed`);
          reject(error);
          for (const call of this.pending.values()) call.reject(error);
          this.pending.clear();
        };
      });
    }
    async call(op, fields = {}, transfer = []) {
      await this.ready;
      const id = this.nextId++;
      return new Promise((resolve, reject) => {
        this.pending.set(id, { resolve, reject });
        this.worker.postMessage({ id, op, ...fields }, transfer);
      });
    }
    terminate() { this.worker.terminate(); }
  }

  // The host's turn detector (crates/sail-dialogue/src/vad.rs) with the same thresholds,
  // so automatic and manual turns behave the same whichever side runs the VAD.
  class TurnDetector {
    constructor() { this.configure({}); this.reset(); }
    configure({ mode = 'automatic', pauseMs = 3000, maxRecordingMs = 180000 }) {
      this.mode = mode; this.pauseSamples = pauseMs * 16; this.limitSamples = Math.max(1000, Math.min(maxRecordingMs, 180000)) * 16;
    }
    reset() {
      this.speaking = false; this.full = false; this.audio = []; this.length = 0;
      this.speechSamples = 0; this.silenceSamples = 0; this.padding = [];
    }
    // Returns 'start', 'end', 'limit' or null. `frame` is 512 Int16 samples.
    feed(frame, probability) {
      if (this.full) return null;
      const threshold = this.speaking ? 0.35 : 0.5;
      let event = null;
      if (probability >= threshold) {
        if (!this.length) for (const f of this.padding) this.push(f);
        this.push(frame); this.speechSamples += frame.length; this.silenceSamples = 0;
        if (!this.speaking && this.speechSamples >= 128 * 16) { this.speaking = true; event = 'start'; }
      } else if (this.speaking) {
        this.push(frame); this.silenceSamples += frame.length;
        if (this.mode === 'automatic' && this.silenceSamples >= this.pauseSamples) event = 'end';
      } else {
        this.audio = []; this.length = 0; this.speechSamples = 0; this.silenceSamples = 0;
      }
      this.padding.push(frame);
      while (this.padding.length * 512 > 96 * 16) this.padding.shift();
      if (this.speaking && this.length >= this.limitSamples && event !== 'end') { this.full = true; event = 'limit'; }
      return event;
    }
    push(frame) { this.audio.push(frame); this.length += frame.length; }
    get capturedMs() { return Math.floor(this.length / 16); }
    // The captured turn as 16 kHz float samples (bounded by the recording limit); resets.
    take() {
      const out = new Float32Array(Math.min(this.length, this.limitSamples)); let offset = 0;
      for (const frame of this.audio) {
        for (let i = 0; i < frame.length && offset < out.length; i++) out[offset++] = frame[i] / 32768;
      }
      const hadSpeech = this.speaking; this.reset();
      return hadSpeech ? out : null;
    }
  }

  const engine = {
    status: 'off', detail: 'Speech runs on the SAIL host.', device: null, voices: [],
    stt: false, tts: false, measured: null, settled: false,
    // Loading bar: phase 'downloading' (bytes known so far) or 'measuring'; null when idle.
    loading: null,
    vadWorker: null, speechWorker: null, detector: new TurnDetector(),
  };

  function preferred() { return localStorage.getItem(PREF_KEY) !== 'host'; }
  function setPreferred(onDevice) { localStorage.setItem(PREF_KEY, onDevice ? 'device' : 'host'); }

  function stop(detail = 'Speech runs on the SAIL host.') {
    engine.vadWorker?.terminate(); engine.speechWorker?.terminate();
    Object.assign(engine, { status: 'off', detail, device: null, voices: [], stt: false, tts: false, measured: null, vadWorker: null, speechWorker: null, settled: true, loading: null });
    engine.detector.reset();
  }

  // Load the chain if this browser and the host support it. Resolves to the engine state;
  // on any failure the engine stays off and the host workers keep handling speech.
  async function load(onStatus = () => {}) {
    if (!preferred()) { stop('Speech runs on the SAIL host (chosen in settings).'); onStatus(engine); return engine; }
    if (engine.status === 'ready' || engine.status === 'loading') return engine;
    if (typeof Worker === 'undefined' || typeof WebAssembly === 'undefined') {
      stop('This browser cannot run on-device speech; the SAIL host handles it.'); onStatus(engine); return engine;
    }
    try {
      const manifest = await fetch('/voice-assets/manifest.json', { cache: 'no-cache' });
      if (!manifest.ok) { stop('On-device speech is not installed on the SAIL host; the host handles speech.'); onStatus(engine); return engine; }
      const files = new Map(), expected = new Map();
      engine.loading = { phase: 'downloading', device: null, loaded: 0, total: 0 };
      Object.assign(engine, { status: 'loading', settled: false, detail: 'Loading speech models on this device…' }); onStatus(engine);
      let lastReport = 0;
      const onProgress = progress => {
        if (progress.device) engine.loading.device = progress.device;
        if (progress.expected) expected.set(progress.role, progress.expected);
        if (progress.file) {
          // Keep a finished file's bytes even if its 'done' report has no size.
          const previous = files.get(progress.file);
          files.set(progress.file, progress.done && !progress.total && previous ? { ...previous, loaded: previous.total, done: true } : progress);
        }
        let loaded = 0;
        for (const file of files.values()) loaded += file.loaded;
        // Measure only against the whole download, once both workers have declared it;
        // until then (or with an older manifest without sizes) the bar is indeterminate,
        // so it never runs backwards.
        const total = expected.size === 2 ? [...expected.values()].reduce((a, b) => a + b, 0) : 0;
        Object.assign(engine.loading, { loaded, total });
        const now = performance.now();
        if (now - lastReport > 100 || progress.done || progress.device) { lastReport = now; onStatus(engine); }
      };
      engine.vadWorker = new EngineWorker('vad', onProgress); engine.speechWorker = new EngineWorker('speech', onProgress);
      const [, speech] = await Promise.all([engine.vadWorker.call('load'), engine.speechWorker.call('load')]);
      engine.loading = { phase: 'measuring', device: speech.device, loaded: 0, total: 0 }; onStatus(engine);
      const measured = await benchmark();
      engine.loading = null;
      const limit = maxRealTimeFactor();
      const stt = measured.sttRtf <= limit, tts = measured.ttsRtf <= limit;
      const where = speech.device === 'webgpu' ? 'WebGPU listening, CPU voice' : 'CPU, WebAssembly';
      const speed = `measured ${measured.sttRtf.toFixed(2)}× real time to transcribe, ${measured.ttsRtf.toFixed(2)}× to speak`;
      if (!stt && !tts) {
        stop(`This device is too slow for speech (${where}, ${speed}); the SAIL host handles it.`);
        onStatus(engine); return engine;
      }
      const parts = [stt && 'listening and transcription', tts && 'the spoken voice'].filter(Boolean).join(' and ');
      Object.assign(engine, {
        status: 'ready', settled: true, device: speech.device, voices: speech.voices, stt, tts, measured,
        detail: `On this device (${where}): ${parts}; ${speed}.${stt && tts ? '' : ' The rest runs on the SAIL host.'}`,
      });
    } catch (error) {
      stop(`On-device speech failed to load (${error.message}); the SAIL host handles speech.`);
    }
    onStatus(engine);
    return engine;
  }

  // Speak a short phrase and transcribe it back; also warms both models.
  async function benchmark() {
    const t0 = performance.now();
    const speech = await engine.speechWorker.call('speak', { text: WARM_UP, voice: 'af_heart' });
    const ttsMs = performance.now() - t0, seconds = speech.audio.length / speech.sampling_rate;
    const samples = resample(speech.audio, speech.sampling_rate);
    const t1 = performance.now();
    await engine.speechWorker.call('transcribe', { samples }, [samples.buffer]);
    const sttMs = performance.now() - t1;
    return { ttsRtf: ttsMs / 1000 / seconds, sttRtf: sttMs / 1000 / seconds, seconds };
  }
  function resample(audio, rate) {
    const ratio = rate / 16000, out = new Float32Array(Math.floor(audio.length / ratio));
    for (let i = 0; i < out.length; i++) out[i] = audio[Math.floor(i * ratio)];
    return out;
  }

  const ready = () => engine.status === 'ready';
  return {
    engine, load, stop, ready, preferred, setPreferred,
    sttOnDevice: () => ready() && engine.stt,
    ttsOnDevice: voice => ready() && engine.tts && engine.voices.includes(voice),
    resample,
    configure: settings => engine.detector.configure(settings),
    resetCapture: () => { engine.detector.reset(); engine.vadWorker?.call('reset').catch(() => {}); },
    // One 512-sample Int16 frame: { event, probability, inferenceMs, capturedMs }.
    async frame(int16) {
      const samples = new Float32Array(int16.length);
      for (let i = 0; i < int16.length; i++) samples[i] = int16[i] / 32768;
      const { probability, inference_ms } = await engine.vadWorker.call('vad', { samples }, [samples.buffer]);
      const event = engine.detector.feed(int16, probability);
      return { event, probability, inferenceMs: inference_ms, capturedMs: engine.detector.capturedMs };
    },
    takeCapture: () => engine.detector.take(),
    async transcribe(samples) { return engine.speechWorker.call('transcribe', { samples }, [samples.buffer]); },
    async speak(text, voice) { return engine.speechWorker.call('speak', { text, voice }); },
  };
})();
