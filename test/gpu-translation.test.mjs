import assert from 'node:assert/strict';
import test from 'node:test';
import { GpuExecutor } from '../web/gpu.js';

test('unsupported DXGI formats are exposed instead of substituted with RGBA8', () => {
  const gpu = new GpuExecutor({}, null);
  gpu.features = new Set();

  assert.equal(gpu.texFormat(28), 'rgba8unorm');
  assert.throws(() => gpu.texFormat(0xdead), /unsupported DXGI format 0xdead/);
  assert.throws(() => gpu.vertexFormat(0xdead), /unsupported DXGI vertex format 0xdead/);
});

test('unsupported texture compression and depth features are exposed', () => {
  const gpu = new GpuExecutor({}, null);
  gpu.features = new Set();

  assert.throws(() => gpu.texFormat(71), /DXGI format 71 requires WebGPU feature texture-compression-bc/);
  assert.throws(() => gpu.texFormat(20), /DXGI format 20 requires WebGPU feature depth32float-stencil8/);
});

test('execute returns renderer translation errors to its caller', () => {
  const gpu = new GpuExecutor({}, null);
  const command = new Uint32Array([0xffff, 0]);

  const errors = gpu.execute(new Uint8Array(command.buffer));

  assert.deepEqual(errors, ['op 65535: unsupported GPU opcode 65535']);
  assert.deepEqual(gpu.stats.errors, errors);
});

test('unsupported multisampled array SRVs are exposed instead of flattened to 2D', () => {
  const gpu = new GpuExecutor({}, null);
  gpu.features = new Set();
  gpu.res.set(4, {
    kind: 'texture',
    format: 'rgba8unorm',
    dxgi: 28,
    bind: 0,
    dimension: '2d',
    samples: 4,
    array: 2,
    mips: 1,
    multisampledLayers: [{ createView: (desc) => desc }, { createView: (desc) => desc }],
  });
  gpu.views.set(7, {
    kind: 2,
    resource: 4,
    format: 28,
    dim: 7,
    mip: 0,
    mips: 1,
    slice: 0,
    slices: 2,
  });

  assert.throws(() => gpu.getView(7), /multisampled array SRV dimension 7 is not translated/);
});

test('malformed PRESENT gamma payloads are returned as translation errors', () => {
  const gpu = new GpuExecutor({}, null);
  const command = new Uint32Array([35, 16, 1, 2, 0, 0]);

  const errors = gpu.execute(new Uint8Array(command.buffer));

  assert.deepEqual(errors, ['op 35: PRESENT gamma ramp payload has 4 bytes; expected 1536']);
});

test('presenting an untranslated backbuffer is exposed', () => {
  const gpu = new GpuExecutor({}, null);

  assert.throws(() => gpu.present(1, 99), /backbuffer resource 99 is not translated/);
});

test('truncated GPU command headers and payloads are exposed', () => {
  const headerGpu = new GpuExecutor({}, null);
  const headerErrors = headerGpu.execute(new Uint8Array([35, 0]));
  assert.deepEqual(headerErrors, ['truncated GPU command header at offset 0: 2 bytes; expected 8']);

  const payloadGpu = new GpuExecutor({}, null);
  const command = new Uint32Array([0xffff, 4]);
  const payloadErrors = payloadGpu.execute(new Uint8Array(command.buffer));
  assert.deepEqual(payloadErrors, [
    'truncated GPU command 65535 at offset 0: declared payload 4 bytes, only 0 remain',
  ]);
});

test('GDI fill rectangle renderer rejects window targets that were not created', () => {
  const gpu = new GpuExecutor({}, null);
  const command = new Uint32Array([54, 24, 7, 0, 0, 10, 10, 0x00030201]);

  const errors = gpu.execute(new Uint8Array(command.buffer));

  assert.deepEqual(errors, ['op 54: GDI FillRect window 7 has not been created']);
});
