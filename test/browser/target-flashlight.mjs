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

export function mapFlashlightOrigin(marker, screenWidth, screenHeight, sdfWidth, sdfHeight) {
  if (!Number.isFinite(marker?.x) || !Number.isFinite(marker?.y) ||
      !Number.isFinite(screenWidth) || screenWidth <= 0 ||
      !Number.isFinite(screenHeight) || screenHeight <= 0 ||
      !Number.isInteger(sdfWidth) || sdfWidth <= 0 ||
      !Number.isInteger(sdfHeight) || sdfHeight <= 0) {
    throw new Error('player marker and screen/SDF dimensions are required to map the flashlight origin');
  }
  return {
    originX: Math.max(0, Math.min(sdfWidth - 1, Math.round(marker.x * sdfWidth / screenWidth))),
    originY: Math.max(0, Math.min(sdfHeight - 1, Math.round(marker.y * sdfHeight / screenHeight))),
  };
}

export function selectClearFlashlightDirection(values, width, height, {
  originX = Math.floor(width / 2),
  originY = Math.floor(height / 2),
} = {}) {
  if (!Number.isInteger(width) || width <= 0 || !Number.isInteger(height) || height <= 0 ||
      !values || values.length !== width * height) {
    throw new Error('flashlight SDF dimensions must match its values');
  }
  if (!Number.isInteger(originX) || originX < 0 || originX >= width ||
      !Number.isInteger(originY) || originY < 0 || originY >= height) {
    throw new Error('flashlight SDF origin must be inside the texture');
  }

  let best = null;
  for (const direction of DIRECTIONS) {
    let clearTexels = 0;
    let minimumClearance = Infinity;
    for (let step = 1; ; step++) {
      const x = originX + direction.dx * step;
      const y = originY + direction.dy * step;
      if (x < 0 || x >= width || y < 0 || y >= height) break;
      const clearance = Number(values[y * width + x]);
      if (!Number.isFinite(clearance)) throw new Error('flashlight SDF values must be finite');
      if (clearance <= 0) break;
      clearTexels++;
      minimumClearance = Math.min(minimumClearance, clearance);
    }

    if (!best || clearTexels > best.clearTexels ||
        clearTexels === best.clearTexels && minimumClearance > best.minimumClearance) {
      best = { ...direction, clearTexels, minimumClearance };
    }
  }

  if (!best || best.clearTexels === 0) {
    throw new Error('flashlight SDF has no clear flashlight ray from its origin');
  }
  return best;
}
