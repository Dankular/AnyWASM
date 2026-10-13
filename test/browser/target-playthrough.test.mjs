import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import * as playthrough from './target-playthrough.mjs';

const { buildPlaythroughPlan } = playthrough;

const record = JSON.parse(readFileSync(
  new URL('../../docs/target-data/glfwdemo-seed1-playthrough.json', import.meta.url),
  'utf8',
));

test('reduces pulse scale when a segment travels farther than its recorded displacement', () => {
  assert.equal(playthrough.recalibratePulseScale(1, 10, 20), 0.5);
});

test('splits long movement segments into bounded pulse batches', () => {
  assert.deepEqual(playthrough.splitPulseCount(17, 8), [8, 8, 1]);
});

test('builds a timed key plan from the recorded seed-1 route', () => {
  const plan = buildPlaythroughPlan(record, { pulseMs: 10 });

  assert.equal(plan.steps.length, 35);
  assert.deepEqual(plan.steps[0], {
    keys: ['d'],
    durationMs: 250,
    pulseCount: 25,
    expectedDistance: 0.625,
    expectedBefore: { x: 519.5, y: 403 },
    expectedAfter: { x: 527.2105263157895, y: 403 },
  });
  assert.equal(plan.keyPulses, 1834);
  assert.equal(plan.teleported, true);
});

test('scales recorded marker coordinates to the current guest back-buffer size', () => {
  const sizedRecord = { ...record, captureSize: { width: 1040, height: 807 } };
  const plan = buildPlaythroughPlan(sizedRecord, { pulseMs: 10, targetWidth: 1024, targetHeight: 768 });

  assert.ok(Math.abs(plan.steps[0].expectedBefore.x - 511.5) < 0.03);
  assert.ok(Math.abs(plan.steps[0].expectedBefore.y - 383.5) < 0.1);
  assert.ok(Math.abs(plan.steps[0].expectedAfter.x - 518.86) < 0.1);
  assert.equal(plan.steps[0].durationMs, 250);
});

test('maps route coordinates through the centered square gameplay viewport', () => {
  const sizedRecord = {
    captureSize: { width: 1040, height: 807 },
    result: {
      runProgress: [{
        keys: ['d'],
        pulses: 10,
        expectedDistance: 1,
        before: { x: 850, y: 200 },
        after: { x: 860, y: 210 },
      }],
    },
  };
  const plan = buildPlaythroughPlan(sizedRecord, { pulseMs: 10, targetWidth: 1024, targetHeight: 768 });
  const scale = 768 / 807;

  assert.ok(Math.abs(plan.steps[0].expectedBefore.x - (128 + (850 - 116.5) * scale)) < 1e-9);
  assert.ok(Math.abs(plan.steps[0].expectedBefore.y - 200 * scale) < 1e-9);
});

test('rejects a route step without a positive pulse count', () => {
  assert.throws(() => buildPlaythroughPlan({ result: { runProgress: [{ keys: ['w'], pulses: 0 }] } }), /positive integer/);
});
