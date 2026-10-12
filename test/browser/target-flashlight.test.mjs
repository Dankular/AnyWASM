import assert from 'node:assert/strict';
import test from 'node:test';

const flashlightHelpers = await import('./target-flashlight.mjs').catch(() => ({}));

test('selects the longest positive SDF ray from the player origin', () => {
  const width = 9, height = 9;
  const sdf = new Float32Array(width * height).fill(-1);
  const at = (x, y) => y * width + x;
  sdf[at(4, 4)] = 2;
  sdf[at(5, 4)] = 1;
  sdf[at(6, 4)] = 1;
  sdf[at(3, 4)] = 1;
  sdf[at(4, 3)] = 1;
  sdf[at(4, 2)] = 1;
  sdf[at(4, 1)] = 1;

  assert.equal(typeof flashlightHelpers.selectClearFlashlightDirection, 'function',
    'the flashlight check needs to select an unobstructed SDF ray');
  assert.deepEqual(
    flashlightHelpers.selectClearFlashlightDirection(sdf, width, height),
    { direction: 'up', dx: 0, dy: -1, clearTexels: 3, minimumClearance: 1 },
  );
});

test('rejects an SDF map with no positive ray from the player origin', () => {
  const sdf = new Float32Array(9 * 9).fill(-1);
  sdf[4 * 9 + 4] = 1;

  assert.equal(typeof flashlightHelpers.selectClearFlashlightDirection, 'function',
    'the flashlight check needs to select an unobstructed SDF ray');
  assert.throws(
    () => flashlightHelpers.selectClearFlashlightDirection(sdf, 9, 9),
    /no clear flashlight ray/,
  );
});

test('maps the captured player marker into SDF texel coordinates', () => {
  assert.equal(typeof flashlightHelpers.mapFlashlightOrigin, 'function',
    'the flashlight check needs to map the current player position into the SDF');
  assert.deepEqual(
    flashlightHelpers.mapFlashlightOrigin({ x: 512, y: 360 }, 1024, 768, 128, 128),
    { originX: 64, originY: 60 },
  );
});
