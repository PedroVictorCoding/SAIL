// Stateful area resampler: the worklet's real hardware rate -> 16kHz mono PCM.
class SailCapture extends AudioWorkletProcessor {
  constructor() {
    super(); this.ratio = sampleRate / 16000; this.weight = 0; this.sum = 0;
    this.frame = new Int16Array(512); this.index = 0;
  }
  process(inputs, outputs) {
    const channels = inputs[0];
    if (!channels || !channels.length) return true;
    for (let i = 0; i < channels[0].length; i++) {
      let mono = 0;
      for (const channel of channels) mono += channel[i] / channels.length;
      let available = 1;
      while (available > 1e-8) {
        const take = Math.min(available, this.ratio - this.weight);
        this.sum += mono * take; this.weight += take; available -= take;
        if (this.weight >= this.ratio - 1e-8) {
          const sample = Math.max(-1, Math.min(1, this.sum / this.ratio));
          this.frame[this.index++] = Math.round(sample * (sample < 0 ? 32768 : 32767));
          this.sum = 0; this.weight = 0;
          if (this.index === 512) {
            // Explicit little-endian transport, independent of host endianness.
            const buffer = new ArrayBuffer(1024), view = new DataView(buffer);
            for (let j = 0; j < 512; j++) view.setInt16(j * 2, this.frame[j], true);
            this.port.postMessage(buffer, [buffer]); this.index = 0;
          }
        }
      }
    }
    for (const output of outputs) for (const channel of output) channel.fill(0);
    return true;
  }
}
registerProcessor('sail-capture', SailCapture);
