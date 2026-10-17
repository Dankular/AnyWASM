// Verifies that the direct Sims 3 host feed reports real changes from the streamed install.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdirSync, mkdtempSync, rmSync, statSync, unlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../', import.meta.url));
const temp = mkdtempSync(join(tmpdir(), 'anywasm-sims3-change-feed-'));
const sims3Install = join(temp, 'sims3-install');
const assets = join(sims3Install, 'Game', 'Bin', 'game', 'assets');
mkdirSync(assets, { recursive: true });
writeFileSync(join(sims3Install, 'Game', 'Bin', 'TS3W.exe'), Buffer.from('MZ'));

const portProbe = createServer();
await new Promise((resolveListen) => portProbe.listen(0, '127.0.0.1', resolveListen));
const port = portProbe.address().port;
await new Promise((resolveClose) => portProbe.close(resolveClose));
const origin = `http://127.0.0.1:${port}`;
const server = spawn(process.execPath, [join(repo, 'web', 'serve.mjs'), String(port)], {
  cwd: repo,
  stdio: 'ignore',
  env: { ...process.env, ANYWASM_SIMS3_ROOT: sims3Install },
});

const delay = (ms) => new Promise((resolveDelay) => setTimeout(resolveDelay, ms));

async function feed(since) {
  const response = await fetch(`${origin}/__local/sims3/changes?since=${since}`, { cache: 'no-store' });
  assert.equal(response.status, 200);
  const value = await response.json();
  assert.equal(value.available, true, value.reason || 'host change feed should remain available');
  return value;
}

async function waitForChange(path, requiredFilter, since) {
  const expected = path.toLowerCase().replaceAll('\\', '/');
  let cursor = since;
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const value = await feed(cursor);
    const change = value.changes.find((entry) =>
      entry.path.toLowerCase().replaceAll('\\', '/') === expected
      && (entry.filter & requiredFilter) === requiredFilter);
    if (change) return { change, sequence: value.latestSequence };
    cursor = value.latestSequence;
    await delay(25);
  }
  throw new Error(`timed out waiting for Sims host change ${path} filter 0x${requiredFilter.toString(16)}`);
}

async function waitForServer() {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error(`local server exited with ${server.exitCode}`);
    try {
      const response = await fetch(`${origin}/__local/sims3/manifest`, { cache: 'no-store' });
      if (response.ok) return response.json();
    } catch {
      // The server may still be starting; retry after the same bounded wait below.
    }
    await delay(50);
  }
  throw new Error('local Sims 3 development server did not start');
}

try {
  const manifest = await waitForServer();
  assert.ok(manifest.files.some((file) => file.path.toLowerCase() === 'game/bin/ts3w.exe'));
  let cursor = manifest.changeSequence;
  assert.equal((await feed(cursor)).available, true);

  const watchedPath = join(assets, 'watched.txt');
  writeFileSync(watchedPath, 'first');
  const created = await waitForChange('Game/Bin/game/assets/watched.txt', 0x0041, cursor);
  cursor = created.sequence;

  writeFileSync(watchedPath, 'second payload');
  const changed = await waitForChange('Game/Bin/game/assets/watched.txt', 0x0008, cursor);
  cursor = changed.sequence;

  const beforeAccess = statSync(watchedPath);
  utimesSync(watchedPath, new Date(beforeAccess.atimeMs + 5000), new Date(beforeAccess.mtimeMs));
  const accessed = await waitForChange('Game/Bin/game/assets/watched.txt', 0x0020, cursor);
  assert.ok(accessed.change.filter & 0x0020, 'last-access-only changes should retain their Windows filter');
  cursor = accessed.sequence;

  unlinkSync(watchedPath);
  await waitForChange('Game/Bin/game/assets/watched.txt', 0x0001, cursor);

  console.log('Sims 3 host change feed passed: create, write/size, last-access, and delete changes are reported.');
} finally {
  server.kill();
  const tempRoot = resolve(tmpdir()) + sep;
  const tempPath = resolve(temp);
  assert.ok(tempPath.startsWith(tempRoot), `refusing to remove test fixture outside temp root: ${tempPath}`);
  rmSync(tempPath, { recursive: true, force: true });
}
