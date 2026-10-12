const PLAYABLE_KEYS = new Set(['w', 'a', 's', 'd']);

export function buildPlaythroughPlan(record, { pulseMs = 10, targetWidth, targetHeight } = {}) {
  if (!Number.isFinite(pulseMs) || pulseMs <= 0) {
    throw new Error('pulseMs must be a positive number');
  }

  let scaleX = 1, scaleY = 1;
  if (targetWidth !== undefined || targetHeight !== undefined) {
    const captureSize = record?.captureSize;
    if (!Number.isFinite(targetWidth) || !Number.isFinite(targetHeight) ||
        !Number.isFinite(captureSize?.width) || !Number.isFinite(captureSize?.height) ||
        captureSize.width <= 0 || captureSize.height <= 0) {
      throw new Error('playthrough scaling requires positive captureSize and target dimensions');
    }
    scaleX = targetWidth / captureSize.width;
    scaleY = targetHeight / captureSize.height;
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
      expectedBefore: { x: step.before.x * scaleX, y: step.before.y * scaleY },
      expectedAfter: { x: step.after.x * scaleX, y: step.after.y * scaleY },
    };
  });

  return {
    steps,
    pulseMs,
    keyPulses: steps.reduce((sum, step, index) => sum + recordedSteps[index].pulses, 0),
    teleported: route.teleported === true,
  };
}
