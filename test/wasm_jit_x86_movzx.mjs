// Differentially run the GTA-observed x86 MOVZX pair through the WASM runtime.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const wasmPath = join(repoRoot, 'target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm');
const cliPath = join(repoRoot, `target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`);
const fixturePath = join(repoRoot, 'fixtures/jit_movzx_x86.exe');
assert.ok(existsSync(fixturePath), `missing x86 MOVZX fixture: ${fixturePath}`);
assert.ok(existsSync(wasmPath), `missing jit-test-api WASM build: ${wasmPath}`);
assert.ok(existsSync(cliPath), `missing AnyWASM CLI: ${cliPath}`);

const executable = readFileSync(fixturePath);
const entryPair = Buffer.from([0x0f, 0xb6, 0x1e, 0x0f, 0xb6, 0xc3]);
const parserLeaPair = Buffer.from([0x8d, 0x04, 0xbf, 0x8d, 0x7c, 0x43, 0xd0]);
assert.equal(executable.readUInt16LE(executable.readUInt32LE(0x3c) + 4), 0x014c,
  'fixture must be a PE32 x86 executable');
assert.notEqual(executable.indexOf(entryPair), -1,
  'fixture must contain adjacent MOVZX EBX,[ESI] and MOVZX EAX,BL instructions');
assert.notEqual(executable.indexOf(parserLeaPair), -1,
  'fixture must contain the scaled LEA pair from the GTA format parser');

const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-movzx-'));
const bundlePath = join(tempDir, 'jit_movzx_x86.awb');
try {
  execFileSync(cliPath, ['pack', fixturePath, '-o', bundlePath], { cwd: repoRoot, stdio: 'ignore' });
  const bundle = readFileSync(bundlePath);

  async function run(options) {
    const runtime = await AnyWasm.create(readFileSync(wasmPath), options);
    runtime.load(bundle);
    let stdout = '';
    let stderr = '';
    const result = runtime.runToEnd({
      onOutput(stream, text) {
        if (stream === 'stdout') stdout += text;
        else stderr += text;
      },
    });
    const cpuSize = 776;
    const cpuPtr = runtime.x.aw_alloc(cpuSize);
    let cpuSnapshot;
    try {
      assert.equal(runtime.x.aw_test_cpu_snapshot(cpuPtr, cpuSize), cpuSize);
      cpuSnapshot = new Uint8Array(runtime.x.memory.buffer, cpuPtr, cpuSize).slice();
    } finally {
      runtime.x.aw_free(cpuPtr, cpuSize);
    }
    const memorySize = runtime.x.aw_test_memory_snapshot(0, 0);
    const memoryPtr = runtime.x.aw_alloc(memorySize);
    let memorySnapshot;
    try {
      assert.equal(runtime.x.aw_test_memory_snapshot(memoryPtr, memorySize), memorySize);
      memorySnapshot = new Uint8Array(runtime.x.memory.buffer, memoryPtr, memorySize).slice();
    } finally {
      runtime.x.aw_free(memoryPtr, memorySize);
    }
    return {
      status: result.status,
      exitCode: result.exitCode,
      stdout,
      stderr,
      retired: runtime.retiredInstructions(),
      cpuSnapshot,
      memorySnapshot,
      jitStats: { ...runtime.jitStats },
    };
  }

  const emitted = await run({ jitMinPrefix: 1 });
  const cached = await run({});
  const interpreted = await run({ jit: false, jitMinPrefix: 1 });
  for (const [name, result] of [['emitted', emitted], ['cached', cached], ['interpreter', interpreted]]) {
    const detail = `${name} result: ${JSON.stringify({
      status: result.status,
      exitCode: result.exitCode,
      stdout: result.stdout,
      stderr: result.stderr,
      jitStats: result.jitStats,
    })}`;
    assert.equal(result.status, Status.EXITED, `${name} status; ${detail}`);
    assert.equal(result.exitCode, 0, `${name} exit code; ${detail}`);
  }
  assert.ok(emitted.jitStats.compilations > 0, 'forced emission must compile a block');
  assert.ok(emitted.jitStats.executions > 0,
    `forced emission must execute a generated block: ${JSON.stringify(emitted.jitStats)}`);
  assert.ok(emitted.jitStats.emittedGuestInstructions >= 9,
    'generated execution must include the MOVZX and scaled-LEA parser block');
  assert.ok(emitted.jitStats.registerRegisterHits > 0,
    'generated execution must retire past MOVZX EAX,BL');
  assert.equal(cached.jitStats.executions, 0, 'cached-only run must not execute emitted blocks');
  assert.equal(interpreted.jitStats.executions, 0, 'interpreter run must not execute emitted blocks');

  for (const candidate of [cached, interpreted]) {
    assert.equal(emitted.status, candidate.status);
    assert.equal(emitted.exitCode, candidate.exitCode);
    assert.equal(emitted.stdout, candidate.stdout);
    assert.equal(emitted.stderr, candidate.stderr);
    assert.equal(emitted.retired, candidate.retired, 'retired instruction count');
    assert.deepEqual(emitted.cpuSnapshot, candidate.cpuSnapshot, 'complete CPU snapshot');
    assert.deepEqual(emitted.memorySnapshot, candidate.memorySnapshot, 'mapped guest memory');
  }

  console.log(JSON.stringify({
    result: 'pass',
    retired: emitted.retired?.toString(),
    jitStats: emitted.jitStats,
    cpuSnapshotBytes: emitted.cpuSnapshot.byteLength,
    mappedMemoryBytes: emitted.memorySnapshot.byteLength,
  }));
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}
