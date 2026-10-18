// WebAudio output sink for the DirectSound PCM event stream emitted by AnyWasm.
export class DSoundAudioOutput {
  constructor(onError = (error) => console.error('[DirectSound]', error)) {
    this.onError = onError;
    this.context = null;
    this.buffers = new Map();
  }

  async activate() {
    const AudioContextClass = globalThis.AudioContext || globalThis.webkitAudioContext;
    if (!AudioContextClass) return false;
    try {
      this.context ||= new AudioContextClass();
      if (this.context.state !== 'running') await this.context.resume();
      return this.context.state === 'running';
    } catch (error) {
      this.onError(new Error(`WebAudio could not be activated: ${error?.message || error}`));
      return false;
    }
  }

  consume(commands) {
    try {
      const bytes = commands instanceof Uint8Array ? commands : new Uint8Array(commands);
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      let offset = 0;
      while (offset < bytes.byteLength) {
        if (offset + 52 > bytes.byteLength) throw new Error('truncated DirectSound event header');
        const length = view.getUint32(offset, true);
        if (length < 52 || offset + length > bytes.byteLength) throw new Error('invalid DirectSound event length');
        const command = {
          kind: view.getUint32(offset + 4, true),
          id: view.getBigUint64(offset + 8, true).toString(),
          sampleRate: view.getUint32(offset + 16, true),
          frequency: view.getUint32(offset + 20, true),
          volume: view.getInt32(offset + 24, true),
          looping: view.getUint32(offset + 28, true) !== 0,
          cursor: view.getUint32(offset + 32, true),
          startedAtUs: view.getBigUint64(offset + 36, true),
          channels: view.getUint16(offset + 44, true),
          bits: view.getUint16(offset + 46, true),
          sampleBytes: view.getUint32(offset + 48, true),
          samplesOffset: offset + 52,
        };
        if (command.samplesOffset + command.sampleBytes !== offset + length) {
          throw new Error('DirectSound event sample data length does not match its record');
        }
        if (command.kind === 1) {
          this.#play(command, bytes.subarray(command.samplesOffset, command.samplesOffset + command.sampleBytes));
        } else if (command.kind === 2 || command.kind === 5) {
          this.#stop(command.id);
          if (command.kind === 5) this.buffers.delete(command.id);
        } else if (command.kind === 3) {
          this.#update(command);
        } else if (command.kind === 4) {
          const current = this.buffers.get(command.id);
          if (current) this.#start(command.id, current, command.cursor, BigInt(Math.floor(performance.now() * 1000)));
        } else {
          throw new Error(`unknown DirectSound event type ${command.kind}`);
        }
        offset += length;
      }
    } catch (error) {
      this.onError(error instanceof Error ? error : new Error(String(error)));
    }
  }

  #play(command, samples) {
    if (!this.context || this.context.state !== 'running') {
      throw new Error('DirectSound playback event arrived without an active WebAudio output device');
    }
    if (!Number.isInteger(command.sampleRate) || command.sampleRate < 3000 || command.sampleRate > 384000 ||
        ![1, 2].includes(command.channels) || ![8, 16].includes(command.bits)) {
      throw new Error(`unsupported DirectSound PCM format (${command.channels} channels, ${command.bits} bits, ${command.sampleRate} Hz)`);
    }
    const bytesPerSample = command.bits / 8;
    const blockAlign = command.channels * bytesPerSample;
    if (!samples.byteLength || samples.byteLength % blockAlign !== 0) {
      throw new Error('DirectSound PCM buffer has an invalid frame length');
    }
    const frameCount = samples.byteLength / blockAlign;
    const audioBuffer = this.context.createBuffer(command.channels, frameCount, command.sampleRate);
    const sampleView = new DataView(samples.buffer, samples.byteOffset, samples.byteLength);
    for (let channel = 0; channel < command.channels; channel++) {
      const output = audioBuffer.getChannelData(channel);
      for (let frame = 0; frame < frameCount; frame++) {
        const sourceOffset = (frame * command.channels + channel) * bytesPerSample;
        output[frame] = command.bits === 8
          ? (samples[sourceOffset] - 128) / 128
          : sampleView.getInt16(sourceOffset, true) / 32768;
      }
    }
    const state = {
      audioBuffer,
      sampleRate: command.sampleRate,
      bytesPerFrame: command.channels * bytesPerSample,
      frequency: command.frequency,
      volume: command.volume,
      looping: command.looping,
    };
    this.buffers.set(command.id, state);
    this.#start(command.id, state, command.cursor, command.startedAtUs);
  }

  #start(id, state, cursor, startedAtUs) {
    if (!this.context || this.context.state !== 'running') {
      throw new Error('DirectSound seek/play requested without an active WebAudio output device');
    }
    this.#stop(id);
    const source = this.context.createBufferSource();
    const gain = this.context.createGain();
    source.buffer = state.audioBuffer;
    source.loop = state.looping;
    source.playbackRate.value = state.frequency / state.sampleRate;
    gain.gain.value = 10 ** (state.volume / 2000);
    source.connect(gain);
    gain.connect(this.context.destination);
    const offset = cursor / (state.sampleRate * state.bytesPerFrame) * state.audioBuffer.sampleRate;
    const elapsedSeconds = Math.max(0, Number(BigInt(Math.floor(performance.now() * 1000)) - startedAtUs) / 1_000_000);
    const mediaOffset = offset + elapsedSeconds * state.frequency;
    const duration = state.audioBuffer.duration;
    if (!state.looping && mediaOffset / state.sampleRate >= duration) return;
    const startOffset = state.looping
      ? ((mediaOffset / state.sampleRate) % duration)
      : (mediaOffset / state.sampleRate);
    source.onended = () => {
      if (this.buffers.get(id)?.source === source) this.buffers.get(id).source = null;
      source.disconnect();
      gain.disconnect();
    };
    state.source = source;
    state.gain = gain;
    source.start(this.context.currentTime + 0.005, startOffset);
  }

  #stop(id) {
    const state = this.buffers.get(id);
    if (!state?.source) return;
    const { source, gain } = state;
    state.source = null;
    try { source.stop(); } catch { /* stop may race a natural end */ }
    source.disconnect();
    gain?.disconnect();
  }

  #update(command) {
    const state = this.buffers.get(command.id);
    if (!state) return;
    state.frequency = command.frequency;
    state.volume = command.volume;
    if (state.source) state.source.playbackRate.setValueAtTime(command.frequency / state.sampleRate, this.context.currentTime);
    if (state.gain) state.gain.gain.setValueAtTime(10 ** (command.volume / 2000), this.context.currentTime);
  }

  stopAll() {
    for (const [id, state] of this.buffers) this.#stop(id);
    this.buffers.clear();
  }
}
