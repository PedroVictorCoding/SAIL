'use strict';
// Deterministic source text -> bounded PCM queue. No DOM, network or model routing.
const SailReaderAudio = (() => {
  function safeCut(text, offset) {
    // Anchors use UTF-16 offsets, but never divide a surrogate pair.
    if (offset > 0 && offset < text.length && /[\uDC00-\uDFFF]/.test(text[offset]) && /[\uD800-\uDBFF]/.test(text[offset - 1]))
      return offset - 1;
    return offset;
  }
  function segment(blocks, boundaries = [], budget = 320, section = 0) {
    budget = Math.max(2, Math.min(1000, Math.floor(budget) || 320));
    const result = [];
    blocks.forEach((block, b) => {
      const text = block.text;
      const cuts = [
        ...new Set([
          0, ...boundaries.filter(a => a.block === b && a.char > 0 && a.char < text.length).map(a => safeCut(text, a.char)), text.length
        ])
      ].sort((a, c) => a - c);
      for (let c = 0; c < cuts.length - 1; c++) {
        let start = cuts[c];
        while (start < cuts[c + 1]) {
          const limit = safeCut(text, Math.min(start + budget, cuts[c + 1]));
          const part = text.slice(start, limit);
          const sentence = /[.!?。！？](?:["'”’])?(?:\s|$)/u.exec(part);
          let end = sentence ? start + sentence.index + sentence[0].length : limit;
          if (!sentence && limit < cuts[c + 1]) {
            const space = part.lastIndexOf(' ');
            if (space > budget / 2)
              end = start + space + 1;
          }
          if (text.slice(start, end).trim())
            result.push({ text: text.slice(start, end), anchor: { section, block: b, char: start }, end });
          start = end;
        }
      }
    });
    return result;
  }
  // Serial transport shared across generations: an aborted request must settle before
  // another POST starts. Book text only reaches the authenticated reader endpoint.
  class HostEngine {
    constructor({ fetcher, getToken, speed = 1, delay, now = () => performance.now() }) {
      Object.assign(this, { fetcher, getToken, speed, now });
      this.delay = delay || ((signal) => new Promise((resolve, reject) => {
        const timer = setTimeout(resolve, 300);
        signal.addEventListener('abort', () => {
          clearTimeout(timer);
          reject(new Error('Canceled'));
        }, { once: true });
      }));
      this.tail = Promise.resolve();
      this.controller = new AbortController();
      this.backend = 'host';
    }
    async load() {}
    cancel() {
      this.controller.abort();
      this.controller = new AbortController();
    }
    speak(text, voice) {
      const signal = this.controller.signal, speed = this.speed;
      const run = async () => {
        const started = this.now();
        for (;;) {
          if (signal.aborted) throw new Error('Canceled');
          const token = await this.getToken();
          const response = await this.fetcher('/api/reader/speech', {
            method: 'POST', cache: 'no-store', signal,
            headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
            body: JSON.stringify({ text, voice, speed })
          });
          if (response.status === 429) {
            await this.delay(signal);
            continue;
          }
          if (!response.ok) {
            const error = new Error('SAIL host voice unavailable.');
            error.status = response.status;
            throw error;
          }
          const bytes = await response.arrayBuffer();
          if (signal.aborted) throw new Error('Canceled');
          const sampleRate = Number(response.headers.get('X-Sample-Rate'));
          if (bytes.byteLength % 2 || !bytes.byteLength || !(sampleRate > 0))
            throw new Error('Invalid host audio.');
          const view = new DataView(bytes), audio = new Float32Array(bytes.byteLength / 2);
          for (let i = 0; i < audio.length; i++) audio[i] = view.getInt16(i * 2, true) / 32768;
          return { audio, sampling_rate: sampleRate, sampleRate, backend: 'host',
            synthesisMs: Number(response.headers.get('X-Synthesis-Ms')) || this.now() - started };
        }
      };
      const request = this.tail.then(run);
      this.tail = request.catch(() => {});
      return request;
    }
  }
  class VoiceRouter {
    constructor({ host, device, capabilities, preference, onChange = () => {}, now = () => performance.now() }) {
      Object.assign(this, { host, device, capabilities, onChange, now });
      this.backend = capabilities.available && preference !== 'device' ? 'host' : 'device';
      this.slow = 0;
      this.tail = Promise.resolve();
      this.epoch = 0;
    }
    cancel() { this.epoch++; this.host.cancel(); }
    async load() {
      if (this.backend === 'device') await this.device.load();
    }
    async select(backend, message = '') {
      if (backend === 'host' && !this.capabilities.available) throw new Error('Host voice unavailable.');
      const epoch = this.epoch;
      if (backend === 'device') await this.device.load();
      if (epoch !== this.epoch) throw new Error('Canceled');
      this.backend = backend;
      this.slow = 0;
      this.switchPending = false;
      this.onChange(backend, message);
    }
    speak(text, voice) {
      const epoch = this.epoch;
      const run = async () => {
        if (epoch !== this.epoch) throw new Error('Canceled');
        const started = this.now();
        if (this.backend === 'host') {
          try {
            const voices = this.capabilities.voices;
            const selected = !voices || voices.includes(voice)
              ? voice : this.capabilities.default_voice || voices[0];
            return await this.host.speak(text, selected);
          }
          catch (error) {
            if (epoch !== this.epoch) throw error;
            if (error.status && error.status !== 503) throw error;
            this.capabilities.available = false;
            await this.select('device', 'SAIL host voice unavailable; using this device.');
            if (epoch !== this.epoch) throw new Error('Canceled');
          }
        }
        const pcm = await this.device.speak(text, voice);
        if (epoch === this.epoch) {
          this.slow = this.now() - started > pcm.audio.length / pcm.sampling_rate * 1000 ? this.slow + 1 : 0;
          if (this.slow >= 2 && this.capabilities.available) this.switchPending = true;
        }
        return pcm;
      };
      const request = this.tail.then(run);
      this.tail = request.catch(() => {});
      return request;
    }
  }
  class Narrator {
    constructor({ tts, clock, onState = () => {
    }, onSegment = () => {
    }, onSlow = () => {}, now = () => performance.now(), loadMore = async () => [], prefetch = 2 }) {
      Object.assign(this, { tts, clock, onState, onSegment, onSlow, now, loadMore });
      this.ticket = 0;
      this.ahead = Math.max(1, Math.min(3, Math.floor(prefetch) || 2));
      this.generation = 0;
      this.state = 'idle';
      this.follow = true;
      this.auto = true;
      this.cache = new Map();
      this.pending = new Map();
      this.index = 0;
      this.segments = [];
    }
    setState(state) {
      this.state = state;
      this.onState(state);
    }
    setText(segments) {
      this.cancel();
      this.segments = segments;
      this.index = 0;
      this.hasPlayed = false;
      this.setState('text_ready');
    }
    async play() {
      if (['playing', 'tts_loading', 'buffering'].includes(this.state))
        return;
      const generation = this.generation, ticket = ++this.ticket;
      if (this.state === 'paused' && this.playback) {
        this.setState('buffering');
        try {
          await this.clock.resume();
          if (generation === this.generation && this.state !== 'paused')
            this.setState('playing');
        }
        catch (_) {
          if (generation === this.generation)
            this.setState('error');
        }
        return;
      }
      this.setState('tts_loading');
      try {
        await this.tts.load();
        if (generation !== this.generation || ticket !== this.ticket || this.state === 'paused')
          return;
        this.setState('buffering');
        await this.next(generation);
      }
      catch (_) {
        if (generation === this.generation && ticket === this.ticket && this.state !== 'paused') {
          this.clock.stop();
          this.setState('error');
        }
      }
    }
    fill() {
      const generation = this.generation;
      for (let i = this.index; i < Math.min(this.segments.length, this.index + this.ahead + 1); i++) {
        if (this.cache.has(i) || this.pending.has(i))
          continue;
        // Start timing when synthesis is requested, excluding time spent in the PCM cache.
        let started;
        const request = Promise.resolve().then(() => {
          if (generation !== this.generation)
            return null;
          started = this.now();
          return this.tts.speak(this.segments[i].text, this.voice);
        }).then(pcm => {
          const duration = pcm?.audio.length / pcm?.sampling_rate * 1000;
          if (generation === this.generation && duration > 0
            && this.now() - started > duration && !this.slowReported) {
            this.slowReported = true;
            this.onSlow();
          }
          if (generation === this.generation)
            this.cache.set(i, pcm);
          return pcm;
        });
        this.pending.set(i, request);
        request.catch(() => {
        });
      }
    }
    prefetchSection(generation) {
      if (this.continuation)
        return;
      const last = this.segments.at(-1);
      if (!last)
        return;
      // Fetch only at the final segment's playback start, never during synthesis.
      this.continuationEnd = this.segments.length;
      this.continuation = Promise.resolve().then(() => this.loadMore(last, generation)).then(more => {
        if (generation !== this.generation)
          return;
        this.segments.push(...more);
        // The normal queue bound also applies across section boundaries.
        if (this.state !== 'paused')
          this.fill();
      });
      this.continuation.catch(() => {
      });
    }
    async next(generation) {
      const ticket = ++this.ticket;
      if (generation !== this.generation || this.state === 'paused')
        return;
      if (this.continuation && this.index >= this.continuationEnd) {
        await this.continuation;
        if (generation !== this.generation || ticket !== this.ticket || this.state === 'paused')
          return;
        this.continuation = null;
      }
      if (this.index >= this.segments.length) {
        this.playback = null;
        this.setState('completed');
        return;
      }
      if (this.tts.switchPending) {
        await this.tts.select('host',
          'Switched to the SAIL host voice: this device was slower than real time');
        if (generation !== this.generation || ticket !== this.ticket) return;
        return this.seek(this.index, this.state !== 'paused');
      }
      this.fill();
      const pcm = this.cache.get(this.index) || await this.pending.get(this.index);
      if (generation !== this.generation || ticket !== this.ticket || this.state === 'paused')
        return;
      const i = this.index;
      this.playback = this.clock.play(pcm, () => {
        if (generation !== this.generation || this.state === 'paused')
          return;
        this.hasPlayed = true;
        this.setState('playing');
        this.onSegment(this.segments[i], this.follow && this.auto);
        if (i === this.segments.length - 1)
          this.prefetchSection(generation);
      }, () => {
        if (generation !== this.generation)
          return;
        this.cache.delete(i);
        this.pending.delete(i);
        this.index = i + 1;
        this.playback = null;
        if (this.state !== 'paused') {
          this.setState('buffering');
          this.next(generation).catch(() => {
            if (generation === this.generation && this.state !== 'paused')
              this.setState('error');
          });
        }
      });
    }
    pause() {
      if (['playing', 'buffering', 'tts_loading'].includes(this.state)) {
        this.ticket++;
        this.clock.pause();
        this.setState('paused');
      }
    }
    seek(index, autoplay = true) {
      this.cancel();
      this.index = Math.max(0, Math.min(index, this.segments.length));
      this.setState('seeking');
      if (autoplay)
        return this.play();
      this.setState('paused');
    }
    setSpeed(speed) {
      this.tts.host.speed = speed;
      return this.seek(this.index, this.state !== 'paused');
    }
    setVoice(voice) {
      const autoplay = this.state !== 'paused';
      this.voice = voice;
      return this.seek(this.index, autoplay);
    }
    manualNavigation() {
      this.follow = false;
    }
    cancel() {
      this.generation++;
      this.tts.cancel?.();
      this.continuation = null;
      this.clock.stop();
      this.playback = null;
      this.cache.clear();
      this.pending.clear();
      this.setState('canceled');
    }
  }
  class AudioClock {
    constructor(context, onError = () => {
    }) {
      this.context = context;
      this.onError = onError;
      this.paused = false;
    }
    async resume() {
      this.paused = false;
      await this.context.resume();
      if (!this.paused)
        this.begin?.();
    }
    pause() {
      this.paused = true;
      this.context.suspend().catch(this.onError);
    }
    stop() {
      this.begin = null;
      if (this.source) {
        this.source.onended = null;
        try {
          this.source.stop();
        }
        catch (_) {
        }
        this.source = null;
      }
    }
    play(pcm, start, end) {
      this.stop();
      this.paused = false;
      const buffer = this.context.createBuffer(1, pcm.audio.length, pcm.sampling_rate);
      buffer.copyToChannel(pcm.audio, 0);
      const source = this.context.createBufferSource();
      source.buffer = buffer;
      source.connect(this.context.destination);
      source.onended = () => {
        if (this.source !== source)
          return;
        this.source = null;
        this.begin = null;
        end();
      };
      this.source = source;
      let started = false;
      this.begin = () => {
        if (started || this.source !== source || this.paused || this.context.state !== 'running')
          return;
        started = true;
        source.start(this.context.currentTime);
        start();
      };
      this.context.resume().then(() => this.begin?.()).catch(this.onError);
      return source;
    }
  }
  return { segment, Narrator, AudioClock, HostEngine, VoiceRouter };
})();
if (typeof module !== 'undefined')
  module.exports = SailReaderAudio;
