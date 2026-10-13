const DIRECTIONS = [
  { direction: 'right', dx: 1, dy: 0 },
  { direction: 'down-right', dx: 1, dy: 1 },
  { direction: 'down', dx: 0, dy: 1 },
  { direction: 'down-left', dx: -1, dy: 1 },
  { direction: 'left', dx: -1, dy: 0 },
  { direction: 'up-left', dx: -1, dy: -1 },
  { direction: 'up', dx: 0, dy: -1 },
  { direction: 'up-right', dx: 1, dy: -1 },
];

function mapViewport(screenWidth, screenHeight, mapAspect) {
  const screenAspect = screenWidth / screenHeight;
  const width = screenAspect > mapAspect ? screenHeight * mapAspect : screenWidth;
  const height = screenAspect > mapAspect ? screenHeight : screenWidth / mapAspect;
  return {
    x: (screenWidth - width) / 2,
    y: (screenHeight - height) / 2,
    width,
    height,
  };
}

export function mapFlashlightOrigin(marker, screenWidth, screenHeight, sdfWidth, sdfHeight, { mapAspect = 1 } = {}) {
  if (!Number.isFinite(marker?.x) || !Number.isFinite(marker?.y) ||
      !Number.isFinite(screenWidth) || screenWidth <= 0 ||
      !Number.isFinite(screenHeight) || screenHeight <= 0 ||
      !Number.isInteger(sdfWidth) || sdfWidth <= 0 ||
      !Number.isInteger(sdfHeight) || sdfHeight <= 0 ||
      !Number.isFinite(mapAspect) || mapAspect <= 0) {
    throw new Error('player marker and screen/SDF dimensions are required to map the flashlight origin');
  }
  const viewport = mapViewport(screenWidth, screenHeight, mapAspect);
  return {
    originX: Math.max(0, Math.min(sdfWidth - 1, Math.round((marker.x - viewport.x) * sdfWidth / viewport.width - 0.5))),
    originY: Math.max(0, Math.min(sdfHeight - 1,
      Math.round((1 - (marker.y - viewport.y) / viewport.height) * sdfHeight - 0.5))),
  };
}

export function selectClearFlashlightDirection(values, width, height, {
  originX = Math.floor(width / 2),
  originY = Math.floor(height / 2),
  maxDistanceTexels = Infinity,
} = {}) {
  if (!Number.isInteger(width) || width <= 0 || !Number.isInteger(height) || height <= 0 ||
      !values || values.length !== width * height) {
    throw new Error('flashlight SDF dimensions must match its values');
  }
  if (!Number.isInteger(originX) || originX < 0 || originX >= width ||
      !Number.isInteger(originY) || originY < 0 || originY >= height) {
    throw new Error('flashlight SDF origin must be inside the texture');
  }
  if (!(maxDistanceTexels === Infinity || Number.isFinite(maxDistanceTexels) && maxDistanceTexels > 0)) {
    throw new Error('flashlight maximum distance must be positive');
  }

  let best = null;
  for (const direction of DIRECTIONS) {
    let clearTexels = 0;
    let minimumClearance = Infinity;
    let hitWall = false;
    for (let step = 1; ; step++) {
      const x = originX + direction.dx * step;
      const y = originY + direction.dy * step;
      if (x < 0 || x >= width || y < 0 || y >= height) break;
      const clearance = Number(values[y * width + x]);
      if (!Number.isFinite(clearance)) throw new Error('flashlight SDF values must be finite');
      if (clearance <= 0) {
        hitWall = true;
        break;
      }
      clearTexels++;
      minimumClearance = Math.min(minimumClearance, clearance);
    }

    const wallDistanceTexels = (clearTexels + 1) * Math.hypot(direction.dx, direction.dy);
    if (!hitWall || clearTexels === 0 || wallDistanceTexels > maxDistanceTexels) continue;
    if (!best || wallDistanceTexels > best.wallDistanceTexels ||
        wallDistanceTexels === best.wallDistanceTexels && minimumClearance > best.minimumClearance) {
      best = { ...direction, clearTexels, wallDistanceTexels, minimumClearance };
    }
  }

  if (!best || best.clearTexels === 0) {
    throw new Error('no clear flashlight ray reaches a wall within range');
  }
  return best;
}

export function measureFlashlightRayResponse(before, after, width, height, sdfWidth, sdfHeight, origin, direction, {
  mapAspect = 1,
  firstSampleTexel = 2,
  patchRadius = 1,
  minimumLuminanceDelta = 8,
} = {}) {
  if (!Number.isInteger(width) || width <= 0 || !Number.isInteger(height) || height <= 0 ||
      !Number.isInteger(sdfWidth) || sdfWidth <= 0 || !Number.isInteger(sdfHeight) || sdfHeight <= 0 ||
      before?.length !== width * height * 4 || after?.length !== width * height * 4) {
    throw new Error('flashlight image data must match the screen dimensions');
  }
  if (!Number.isInteger(origin?.originX) || origin.originX < 0 || origin.originX >= sdfWidth ||
      !Number.isInteger(origin?.originY) || origin.originY < 0 || origin.originY >= sdfHeight ||
      !Number.isInteger(direction?.dx) || !Number.isInteger(direction?.dy) ||
      direction.dx === 0 && direction.dy === 0 ||
      !Number.isInteger(direction?.clearTexels) || direction.clearTexels <= firstSampleTexel ||
      !Number.isInteger(firstSampleTexel) || firstSampleTexel < 1 ||
      !Number.isInteger(patchRadius) || patchRadius < 0 || !Number.isFinite(mapAspect) || mapAspect <= 0 ||
      !Number.isFinite(minimumLuminanceDelta) || minimumLuminanceDelta < 0) {
    throw new Error('flashlight ray geometry must contain a measurable clear path');
  }

  const viewport = mapViewport(width, height, mapAspect);
  const sampleLuminance = (pixels, centerX, centerY) => {
    let sum = 0, count = 0;
    for (let y = Math.max(0, centerY - patchRadius); y <= Math.min(height - 1, centerY + patchRadius); y++) {
      for (let x = Math.max(0, centerX - patchRadius); x <= Math.min(width - 1, centerX + patchRadius); x++) {
        const i = (y * width + x) * 4;
        sum += 0.2126 * pixels[i] + 0.7152 * pixels[i + 1] + 0.0722 * pixels[i + 2];
        count++;
      }
    }
    return sum / count;
  };

  const samples = [];
  for (let step = firstSampleTexel; step < direction.clearTexels; step++) {
    const sdfX = origin.originX + direction.dx * step;
    const sdfY = origin.originY + direction.dy * step;
    const x = Math.round(viewport.x + (sdfX + 0.5) * viewport.width / sdfWidth);
    const y = Math.round(viewport.y + (sdfHeight - sdfY - 0.5) * viewport.height / sdfHeight);
    const beforeLuminance = sampleLuminance(before, x, y);
    const afterLuminance = sampleLuminance(after, x, y);
    samples.push({ step, x, y, beforeLuminance, afterLuminance, luminanceDelta: afterLuminance - beforeLuminance });
  }
  const bestSample = samples.reduce((best, sample) => !best || sample.luminanceDelta > best.luminanceDelta ? sample : best, null);
  let illuminatedSampleCount = 0, consecutiveIlluminatedSamples = 0, longestIlluminatedRun = 0;
  for (const sample of samples) {
    if (sample.luminanceDelta > minimumLuminanceDelta) {
      illuminatedSampleCount++;
      consecutiveIlluminatedSamples++;
      longestIlluminatedRun = Math.max(longestIlluminatedRun, consecutiveIlluminatedSamples);
    } else {
      consecutiveIlluminatedSamples = 0;
    }
  }
  return {
    maxLuminanceDelta: bestSample?.luminanceDelta ?? 0,
    bestSample,
    sampleCount: samples.length,
    minimumLuminanceDelta,
    illuminatedSampleCount,
    longestIlluminatedRun,
    samples,
  };
}
