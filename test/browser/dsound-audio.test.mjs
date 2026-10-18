import assert from 'node:assert/strict';
import test from 'node:test';
import { DSoundAudioOutput } from '../../web/dsound-audio.js';

class Param {
  constructor(value = 1) { this.value = value; }
  setValueAtTime(value) { this.value = value; }
}

class Source {
  playbackRate = new Param();
  loop = false;
  connect() {}
  disconnect() {}
  start(_when, offset) { this.offset = offset; }
  stop() { this.onended?.(); }
}

class AudioContextMock {
  state = 'running';
  currentTime = 1;
  destination = {};
  async resume() {}
  createBuffer(channels, frames, sampleRate) {
    return {
      numberOfChannels: channels,
      sampleRate,
      duration: frames / sampleRate,
      channels: Array.from({ length: channels }, () => new Float32Array(frames)),
      getChannelData(channel) { return this.channels[channel]; },
    };
  }
  createBufferSource() { return new Source(); }
  createGain() { return { gain: new Param(), connect() {}, disconnect() {} }; }
}

function event({ kind, id = 7n, sampleRate = 22_050, frequency = 22_050, volume = 0, looping = false, cursor = 0, startedAtUs = BigInt(Math.floor(performance.now() * 1000)), channels = 2, bits = 8, samples = new Uint8Array() }) {
  const bytes = new Uint8Array(52 + samples.byteLength);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, bytes.byteLength, true);
  view.setUint32(4, kind, true);
  view.setBigUint64(8, id, true);
  view.setUint32(16, sampleRate, true);
  view.setUint32(20, frequency, true);
  view.setInt32(24, volume, true);
  view.setUint32(28, looping ? 1 : 0, true);
  view.setUint32(32, cursor, true);
  view.setBigUint64(36, startedAtUs, true);
  view.setUint16(44, channels, true);
  view.setUint16(46, bits, true);
  view.setUint32(48, samples.byteLength, true);
  bytes.set(samples, 52);
  return bytes;
}

test('DirectSound PCM events create, update, seek, stop, and release WebAudio sources', async () => {
  const oldAudioContext = globalThis.AudioContext;
  globalThis.AudioContext = AudioContextMock;
  const errors = [];
  try {
    const output = new DSoundAudioOutput((error) => errors.push(error));
    assert.equal(await output.activate(), true);
    const pcm = event({ kind: 1, volume: -600, looping: true, samples: new Uint8Array([0, 255, 128, 64]) });
    output.consume(pcm);
    assert.equal(errors.length, 0);
    const state = output.buffers.get('7');
    assert.ok(state.source);
    assert.equal(state.source.loop, true);
    assert.equal(state.audioBuffer.channels[0][0], -1);
    assert.equal(state.audioBuffer.channels[1][0], 127 / 128);
    assert.ok(Math.abs(state.gain.gain.value - 10 ** (-600 / 2000)) < 1e-12);

    output.consume(event({ kind: 3, frequency: 11_025, volume: -1000, sampleRate: 0, channels: 0, bits: 0, samples: new Uint8Array() }));
    assert.equal(state.source.playbackRate.value, 0.5);
    assert.ok(Math.abs(state.gain.gain.value - 10 ** (-0.5)) < 1e-12);

    const seekStartedAtUs = BigInt(Math.floor(performance.now() * 1000));
    output.consume(event({ kind: 4, cursor: 2, startedAtUs: seekStartedAtUs, sampleRate: 0, frequency: 0, channels: 0, bits: 0, samples: new Uint8Array() }));
    assert.ok(state.source);
    const seekOffset = 2 / (22_050 * 2);
    const expectedOffset = seekOffset + Math.max(0, Number(BigInt(Math.floor(performance.now() * 1000)) - seekStartedAtUs) / 2_000_000);
    assert.ok(Math.abs(state.source.offset - expectedOffset) < 0.001);
    output.consume(event({ kind: 2, sampleRate: 0, frequency: 0, channels: 0, bits: 0, samples: new Uint8Array() }));
    assert.equal(state.source, null);
    assert.equal(output.buffers.has('7'), true);
    output.consume(event({ kind: 5, sampleRate: 0, frequency: 0, channels: 0, bits: 0, samples: new Uint8Array() }));
    assert.equal(output.buffers.has('7'), false);
    assert.equal(errors.length, 0);
  } finally {
    if (oldAudioContext === undefined) delete globalThis.AudioContext;
    else globalThis.AudioContext = oldAudioContext;
  }
});
