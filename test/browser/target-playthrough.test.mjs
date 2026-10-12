import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { buildPlaythroughPlan } from './target-playthrough.mjs';

const record = JSON.parse(readFileSync(
  new URL('../../docs/target-data/glfwdemo-seed1-playthrough.json', import.meta.url),
  'utf8',
));

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

  assert.ok(Math.abs(plan.steps[0].expectedBefore.x - 511.5) < 0.02);
  assert.ok(Math.abs(plan.steps[0].expectedBefore.y - 383.5) < 0.1);
  assert.ok(Math.abs(plan.steps[0].expectedAfter.x - 519.1) < 0.1);
  assert.equal(plan.steps[0].durationMs, 250);
});

test('rejects a route step without a positive pulse count', () => {
  assert.throws(() => buildPlaythroughPlan({ result: { runProgress: [{ keys: ['w'], pulses: 0 }] } }), /positive integer/);
});
