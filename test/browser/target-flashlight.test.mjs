import assert from 'node:assert/strict';
import test from 'node:test';

const flashlightHelpers = await import('./target-flashlight.mjs').catch(() => ({}));

test('selects the longest clear SDF ray that reaches a wall', () => {
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
    { direction: 'up', dx: 0, dy: -1, clearTexels: 3, wallDistanceTexels: 4, minimumClearance: 1 },
  );
});

test('selects a clear ray that reaches a wall within the flashlight distance', () => {
  const width = 17, height = 17;
  const sdf = new Float32Array(width * height).fill(-1);
  const at = (x, y) => y * width + x;
  sdf[at(8, 8)] = 2;
  for (let x = 9; x <= 12; x++) sdf[at(x, 8)] = 1;
  for (let y = 9; y <= 10; y++) sdf[at(8, y)] = 1;
  for (let y = 1; y <= 7; y++) sdf[at(8, y)] = 1;

  assert.deepEqual(
    flashlightHelpers.selectClearFlashlightDirection(sdf, width, height, {
      originX: 8,
      originY: 8,
      maxDistanceTexels: 6,
    }),
    { direction: 'right', dx: 1, dy: 0, clearTexels: 4, wallDistanceTexels: 5, minimumClearance: 1 },
  );
});

test('rejects clear rays whose first wall is beyond flashlight range', () => {
  const width = 17, height = 17;
  const sdf = new Float32Array(width * height).fill(-1);
  const at = (x, y) => y * width + x;
  sdf[at(8, 8)] = 2;
  for (let y = 1; y <= 7; y++) sdf[at(8, y)] = 1;

  assert.throws(
    () => flashlightHelpers.selectClearFlashlightDirection(sdf, width, height, {
      originX: 8,
      originY: 8,
      maxDistanceTexels: 6,
    }),
    /no clear flashlight ray reaches a wall within range/,
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
    { originX: 64, originY: 68 },
  );
});

test('maps screen markers through the centered square game viewport', () => {
  assert.deepEqual(
    flashlightHelpers.mapFlashlightOrigin({ x: 128, y: 0 }, 1024, 768, 128, 128),
    { originX: 0, originY: 127 },
  );
  assert.deepEqual(
    flashlightHelpers.mapFlashlightOrigin({ x: 896, y: 768 }, 1024, 768, 128, 128),
    { originX: 127, originY: 0 },
  );
});

test('measures lighting along the selected clear flashlight ray', () => {
  const { measureFlashlightRayResponse } = flashlightHelpers;
  assert.equal(typeof measureFlashlightRayResponse, 'function',
    'the flashlight check should measure a local region along its selected ray');
  const width = 80, height = 60;
  const before = new Uint8ClampedArray(width * height * 4);
  const after = new Uint8ClampedArray(width * height * 4);
  for (let i = 3; i < after.length; i += 4) before[i] = after[i] = 255;
  const pixel = (x, y) => (y * width + x) * 4;
  for (const centerX of [48, 51, 54]) {
    for (let y = 28; y <= 30; y++) for (let x = centerX - 1; x <= centerX + 1; x++) {
      after.set([255, 255, 255, 255], pixel(x, y));
    }
  }

  const response = measureFlashlightRayResponse(
    before, after, width, height, 20, 20,
    { originX: 10, originY: 10 },
    { dx: 1, dy: 0, clearTexels: 5 },
  );
  assert.ok(response.maxLuminanceDelta > 0, JSON.stringify(response));
  assert.equal(response.longestIlluminatedRun, 3, JSON.stringify(response));
});

test('one bright patch cannot satisfy the consecutive-ray-light threshold', () => {
  const { measureFlashlightRayResponse } = flashlightHelpers;
  const width = 80, height = 60;
  const before = new Uint8ClampedArray(width * height * 4);
  const after = new Uint8ClampedArray(width * height * 4);
  for (let i = 3; i < after.length; i += 4) before[i] = after[i] = 255;
  for (let y = 28; y <= 30; y++) for (let x = 47; x <= 49; x++) {
    const i = (y * width + x) * 4;
    after.set([255, 255, 255, 255], i);
  }

  const response = measureFlashlightRayResponse(
    before, after, width, height, 20, 20,
    { originX: 10, originY: 10 },
    { dx: 1, dy: 0, clearTexels: 5 },
  );
  assert.equal(response.longestIlluminatedRun, 1, JSON.stringify(response));
});

test('maps increasing SDF rows upward on the screen when measuring a vertical ray', () => {
  const { measureFlashlightRayResponse } = flashlightHelpers;
  const width = 64, height = 64;
  const pixels = new Uint8ClampedArray(width * height * 4);
  const response = measureFlashlightRayResponse(
    pixels, pixels, width, height, 16, 16,
    { originX: 8, originY: 8 },
    { dx: 0, dy: 1, clearTexels: 4 },
  );
  assert.deepEqual(response.samples.map((sample) => sample.y), [22, 18], JSON.stringify(response.samples));
});

test('does not count lighting changes outside the selected flashlight ray', () => {
  const { measureFlashlightRayResponse } = flashlightHelpers;
  const width = 80, height = 60;
  const before = new Uint8ClampedArray(width * height * 4);
  const after = new Uint8ClampedArray(width * height * 4);
  for (let i = 3; i < after.length; i += 4) before[i] = after[i] = 255;
  after.set([255, 255, 255, 255], 5 * width * 4 + 5 * 4);

  const response = measureFlashlightRayResponse(
    before, after, width, height, 20, 20,
    { originX: 10, originY: 10 },
    { dx: 1, dy: 0, clearTexels: 5 },
  );
  assert.equal(response.maxLuminanceDelta, 0, JSON.stringify(response));
  assert.equal(response.longestIlluminatedRun, 0, JSON.stringify(response));
});
