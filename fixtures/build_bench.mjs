// Build small Win64 benchmark workloads and the CLI guest-callback budget regression fixture.
import { copyFileSync, linkSync, mkdtempSync, rmSync, symlinkSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const fixtureDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(fixtureDir);
const target = 'x86_64-pc-windows-msvc';
const sysroot = execFileSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' }).trim();
const rustLld = join(sysroot, 'lib', 'rustlib', target, 'bin', process.platform === 'win32' ? 'rust-lld.exe' : 'rust-lld');
const linkDir = mkdtempSync(join(repoRoot, 'target', 'anywasm-lld-'));
const buildDir = mkdtempSync(join(tmpdir(), 'anywasm-bench-build-'));
const lldLink = join(linkDir, process.platform === 'win32' ? 'lld-link.exe' : 'lld-link');
const source = join(fixtureDir, 'bench_workload.rs');

try {
  try {
    if (process.platform === 'win32') linkSync(rustLld, lldLink);
    else symlinkSync(rustLld, lldLink);
  } catch (error) {
    if (error.code !== 'EXDEV' && error.code !== 'EPERM') throw error;
    copyFileSync(rustLld, lldLink);
  }

  for (const [cfg, name] of [
    ['bench_call', 'bench_call.exe'],
    ['bench_memory', 'bench_memory.exe'],
    ['bench_sse', 'bench_sse.exe'],
    ['jit_sse', 'jit_sse.exe'],
    ['callback_budget', 'callback_budget.exe'],
    ['jit_state', 'jit_state.exe'],
  ]) {
    const output = join(buildDir, name);
    execFileSync('rustc', [
      source,
      '--crate-type', 'bin',
      '--edition', '2021',
      '--target', target,
      '--cfg', cfg,
      '-C', 'opt-level=3',
      '-C', 'panic=abort',
      '-C', 'debuginfo=0',
      '-C', 'strip=symbols',
      '-C', `linker=${lldLink}`,
      '-C', 'link-arg=/subsystem:console',
      '-C', 'link-arg=/entry:mainCRTStartup',
      '-C', 'link-arg=/nodefaultlib',
      '-o', output,
    ], { stdio: 'inherit' });
    copyFileSync(output, join(fixtureDir, name));
    console.log(`built ${name}`);
  }
} finally {
  rmSync(linkDir, { recursive: true, force: true });
  rmSync(buildDir, { recursive: true, force: true });
}
