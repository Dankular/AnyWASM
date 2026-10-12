let instance = null;
let memory = null;
let workerIndex = -1;

onmessage = async (event) => {
  const message = event.data;
  try {
    if (message.type === 'init') {
      memory = message.memory;
      workerIndex = message.workerIndex;
      const result = await WebAssembly.instantiate(message.module, { env: { memory } });
      instance = result instanceof WebAssembly.Instance ? result : result.instance;
      const stack = instance.exports.__stack_pointer;
      const heapBase = instance.exports.__heap_base;
      if (!(stack instanceof WebAssembly.Global) || !(heapBase instanceof WebAssembly.Global)) {
        throw new Error('shared CPU worker module must export __stack_pointer and __heap_base');
      }
      postMessage({
        type: 'ready',
        workerIndex,
        heapBase: Number(heapBase.value) >>> 0,
        initialStackPointer: Number(stack.value) >>> 0,
      });
      return;
    }

    if (message.type === 'set-stack') {
      if (!instance) throw new Error('shared CPU worker is not initialized');
      instance.exports.__stack_pointer.value = message.stackTop >>> 0;
      postMessage({ type: 'stack-ready', workerIndex, stackTop: message.stackTop >>> 0 });
      return;
    }

    if (message.type === 'run') {
      if (!instance || !memory) throw new Error('shared CPU worker is not initialized');
      const result = instance.exports.aw_cpu_worker_slice(
        message.sharedBase >>> 0,
        message.availableBytes >>> 0,
        message.slotIndex >>> 0,
        BigInt(message.trapStart),
        BigInt(message.trapEnd),
        BigInt(message.processExit || 0),
        BigInt(message.threadExit || 0),
      );
      const slotOffset = (message.sharedBase + message.slotOffset) >>> 0;
      const slot = new Int32Array(memory.buffer, slotOffset, 16);
      postMessage({
        type: 'complete',
        workerIndex,
        slotIndex: message.slotIndex >>> 0,
        sequence: message.sequence >>> 0,
        result: result >>> 0,
        state: Atomics.load(slot, 0) >>> 0,
      });
    }
  } catch (error) {
    postMessage({
      type: 'worker-error',
      workerIndex,
      message: error?.message || String(error),
    });
  }
};
