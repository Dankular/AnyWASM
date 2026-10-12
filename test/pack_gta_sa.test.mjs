import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const repo = fileURLToPath(new URL('../', import.meta.url));

function entries(bytes, magic, headerBytes, indexLengthAt, countAt) {
  assert.equal(bytes.toString('ascii', 0, 4), magic);
  const count = bytes.readUInt32LE(countAt);
  let cursor = headerBytes;
  const parsed = [];
  for (let i = 0; i < count; i++) {
    const nameLength = bytes.readUInt16LE(cursor); cursor += 2;
    const name = bytes.toString('utf8', cursor, cursor + nameLength); cursor += nameLength;
    if (magic === 'AWB1') {
      const size = Number(bytes.readBigUInt64LE(cursor)); cursor += 8;
      parsed.push({ name, size });
    } else {
      const offset = Number(bytes.readBigUInt64LE(cursor)); cursor += 8;
      const size = Number(bytes.readBigUInt64LE(cursor)); cursor += 8;
      parsed.push({ name, offset, size });
    }
  }
  if (magic === 'AGP1') assert.equal(cursor, headerBytes + Number(bytes.readBigUInt64LE(indexLengthAt)));
  return parsed;
}

test('GTA packer separates startup binaries from all range-streamed game files', { skip: process.platform !== 'win32' }, () => {
  const install = mkdtempSync(join(tmpdir(), 'anywasm-gta-pack-'));
  const outputRoot = join(repo, 'target', `gta-pack-${process.pid}-${Date.now()}`);
  const awbPath = join(outputRoot, 'gta_sa.awb');
  const agpPath = join(outputRoot, 'gta_sa.agp');
  mkdirSync(join(install, 'audio', 'streams'), { recursive: true });
  mkdirSync(join(install, 'data'), { recursive: true });
  mkdirSync(outputRoot, { recursive: true });
  writeFileSync(join(install, 'gta_sa_compact1.0.exe'), Buffer.from([0x4d, 0x5a, 1]));
  writeFileSync(join(install, 'vorbis.dll'), Buffer.from([1, 2, 3]));
  writeFileSync(join(install, 'gta_sa.exe'), Buffer.from([4]));
  writeFileSync(join(install, 'ReadMe.txt'), Buffer.from('skip'));
  writeFileSync(join(install, 'data', 'handling.cfg'), Buffer.from('handling'));
  writeFileSync(join(install, 'audio', 'streams', 'track.dat'), Buffer.from([9, 8, 7, 6]));

  try {
    execFileSync('powershell.exe', [
      '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(repo, 'scripts', 'pack-gta-sa.ps1'),
      '-InstallDir', install,
      '-Output', relative(repo, awbPath),
      '-AssetOutput', relative(repo, agpPath),
    ], { cwd: repo, encoding: 'utf8' });

    const awb = readFileSync(awbPath);
    const agp = readFileSync(agpPath);
    const bootEntries = entries(awb, 'AWB1', 8, null, 4);
    const assets = entries(agp, 'AGP1', 24, 8, 4);
    const assetBytes = new Map(assets.map(({ name, offset, size }) => [name, agp.subarray(offset, offset + size)]));
    assert.match(awb.toString('latin1'), /exe=gta_sa_compact1\.0\.exe/);
    assert.deepEqual(bootEntries.map((entry) => entry.name), ['__manifest__', 'gta_sa_compact1.0.exe', 'vorbis.dll']);
    assert.deepEqual([...assetBytes.keys()], ['audio\\streams\\track.dat', 'data\\handling.cfg']);
    assert.deepEqual(assetBytes.get('audio\\streams\\track.dat'), Buffer.from([9, 8, 7, 6]));
    assert.deepEqual(assetBytes.get('data\\handling.cfg'), Buffer.from('handling'));
    assert.equal(assets.some(({ name }) => /gta_sa\.exe|ReadMe/i.test(name)), false);
  } finally {
    rmSync(install, { recursive: true, force: true });
    rmSync(outputRoot, { recursive: true, force: true });
  }
});
