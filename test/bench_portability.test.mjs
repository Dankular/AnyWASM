import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const testDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(testDir, '..');

test('WASM benchmark runs with native executable and temporary paths', () => {
  const output = execFileSync(process.execPath, [join(testDir, 'bench.mjs')], {
    cwd: repoRoot,
    encoding: 'utf8',
  });

  for (const workload of ['compute', 'call-heavy', 'memory-heavy', 'sse-float']) {
    assert.match(output, new RegExp(`${workload}:`));
  }
  assert.match(output, /round 0: status=1/);
  assert.match(output, /round 2:/);
});
