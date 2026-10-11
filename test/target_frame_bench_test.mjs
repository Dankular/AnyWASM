import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { measureFrameCosts } from './target_frame_bench.mjs';
import { Status } from '../web/anywasm.js';

const repoRoot = fileURLToPath(new URL('../', import.meta.url));
const tempDir = mkdtempSync(join(tmpdir(), 'anywasm-frame-test-'));
try {
  const bundlePath = join(tempDir, 'd3d11_triangle.awb');
  const cliPath = fileURLToPath(new URL(`../target/debug/anywasm${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
  const exePath = fileURLToPath(new URL('../fixtures/d3d11_triangle.exe', import.meta.url));
  execFileSync(cliPath, ['pack', exePath, '-o', bundlePath], { cwd: repoRoot, stdio: 'ignore' });
  const wasm = readFileSync(fileURLToPath(new URL('../target/wasm32-unknown-unknown/release/anywasm_wasm.wasm', import.meta.url)));
  const bundle = readFileSync(bundlePath);
  const result = await measureFrameCosts(wasm, bundle, {
    frameCount: 4,
    maxGuestInstructions: 2_000_000,
    sliceInstructions: 10_000,
    jitMinPrefix: 65,
  });

  assert.equal(result.frames.length, 3);
  assert.ok(result.frames.every((frame) => frame.guestInstructions > 0 && frame.milliseconds >= 0));
  assert.equal(result.terminal?.status, Status.EXITED);
  assert.equal(result.terminal?.exitCode, 0);

  const bounded = await measureFrameCosts(wasm, bundle, {
    frameCount: 4,
    maxGuestInstructions: 1,
    sliceInstructions: 1,
    jitMinPrefix: 65,
  });
  assert.equal(bounded.frames.length, 0);
  assert.equal(bounded.terminal, null);
  assert.equal(bounded.limitReached, true);
  assert.ok(bounded.totalGuestInstructions <= 1);
  console.log('target frame benchmark records all three D3D11 triangle presents before clean exit');
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}
