const PLAYABLE_KEYS = new Set(['w', 'a', 's', 'd']);

function centeredViewport(width, height, aspect) {
  const screenAspect = width / height;
  const viewportWidth = screenAspect > aspect ? height * aspect : width;
  const viewportHeight = screenAspect > aspect ? height : width / aspect;
  return {
    x: (width - viewportWidth) / 2,
    y: (height - viewportHeight) / 2,
    width: viewportWidth,
    height: viewportHeight,
  };
}

export function splitPulseCount(pulseCount, maxBatchSize) {
  if (!Number.isInteger(pulseCount) || pulseCount <= 0 ||
      !Number.isInteger(maxBatchSize) || maxBatchSize <= 0) {
    throw new Error('pulseCount and maxBatchSize must be positive integers');
  }

  const batches = [];
  for (let remaining = pulseCount; remaining > 0; remaining -= maxBatchSize) {
    batches.push(Math.min(remaining, maxBatchSize));
  }
  return batches;
}

export function recalibratePulseScale(currentScale, expectedPixels, actualProjectedPixels) {
  if (!Number.isFinite(currentScale) || currentScale <= 0 ||
      !Number.isFinite(expectedPixels) || expectedPixels <= 0 ||
      !Number.isFinite(actualProjectedPixels) || actualProjectedPixels <= 0) {
    return currentScale;
  }

  return Math.min(3, Math.max(0.25, currentScale * expectedPixels / actualProjectedPixels));
}

export function buildPlaythroughPlan(record, { pulseMs = 10, targetWidth, targetHeight, mapAspect = 1 } = {}) {
  if (!Number.isFinite(pulseMs) || pulseMs <= 0) {
    throw new Error('pulseMs must be a positive number');
  }

  let scaleX = 1, scaleY = 1, offsetX = 0, offsetY = 0;
  if (targetWidth !== undefined || targetHeight !== undefined) {
    const captureSize = record?.captureSize;
    if (!Number.isFinite(targetWidth) || !Number.isFinite(targetHeight) ||
        !Number.isFinite(captureSize?.width) || !Number.isFinite(captureSize?.height) ||
        captureSize.width <= 0 || captureSize.height <= 0 ||
        !Number.isFinite(mapAspect) || mapAspect <= 0) {
      throw new Error('playthrough scaling requires positive captureSize and target dimensions');
    }
    const sourceViewport = centeredViewport(captureSize.width, captureSize.height, mapAspect);
    const targetViewport = centeredViewport(targetWidth, targetHeight, mapAspect);
    scaleX = targetViewport.width / sourceViewport.width;
    scaleY = targetViewport.height / sourceViewport.height;
    offsetX = targetViewport.x - sourceViewport.x * scaleX;
    offsetY = targetViewport.y - sourceViewport.y * scaleY;
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
      expectedBefore: { x: step.before.x * scaleX + offsetX, y: step.before.y * scaleY + offsetY },
      expectedAfter: { x: step.after.x * scaleX + offsetX, y: step.after.y * scaleY + offsetY },
    };
  });

  return {
    steps,
    pulseMs,
    keyPulses: steps.reduce((sum, step, index) => sum + recordedSteps[index].pulses, 0),
    teleported: route.teleported === true,
  };
}
