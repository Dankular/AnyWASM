const PLAYABLE_KEYS = new Set(['w', 'a', 's', 'd']);

export function buildPlaythroughPlan(record, { pulseMs = 10 } = {}) {
  if (!Number.isFinite(pulseMs) || pulseMs <= 0) {
    throw new Error('pulseMs must be a positive number');
  }

  const route = record?.result;
  const recordedSteps = route?.runProgress;
  if (!Array.isArray(recordedSteps) || recordedSteps.length === 0) {
    throw new Error('playthrough record must contain runProgress steps');
  }

  const steps = recordedSteps.map((step, index) => {
    if (!Array.isArray(step.keys) || step.keys.length === 0 ||
        step.keys.some((key) => !PLAYABLE_KEYS.has(key))) {
      throw new Error(`playthrough step ${index} must contain W/A/S/D keys`);
    }
    if (!Number.isInteger(step.pulses) || step.pulses <= 0) {
      throw new Error(`playthrough step ${index} must have a positive integer pulse count`);
    }
    if (!Number.isFinite(step.expectedDistance) ||
        !Number.isFinite(step.before?.x) || !Number.isFinite(step.before?.y) ||
        !Number.isFinite(step.after?.x) || !Number.isFinite(step.after?.y)) {
      throw new Error(`playthrough step ${index} is missing its expected position`);
    }

    return {
      keys: [...step.keys],
      durationMs: Math.max(1, Math.round(step.pulses * pulseMs)),
      pulseCount: step.pulses,
      expectedDistance: step.expectedDistance,
      expectedBefore: { x: step.before.x, y: step.before.y },
      expectedAfter: { x: step.after.x, y: step.after.y },
    };
  });

  return {
    steps,
    pulseMs,
    keyPulses: steps.reduce((sum, step, index) => sum + recordedSteps[index].pulses, 0),
    teleported: route.teleported === true,
  };
}
