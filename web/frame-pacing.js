const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export const nextFrame = () => (typeof requestAnimationFrame === 'function'
  ? new Promise((resolve) => requestAnimationFrame(() => resolve()))
  : sleep(16));

/** Wait for the vblanks requested by DXGI Present, yielding once without sync for interval zero. */
export async function waitForPresentInterval(syncInterval, waitFrame = nextFrame, yieldTask = () => sleep(0)) {
  const count = Math.min(Number(syncInterval) >>> 0, 4);
  if (count === 0) {
    await yieldTask();
    return 0;
  }
  for (let frame = 0; frame < count; frame++) await waitFrame();
  return count;
}
