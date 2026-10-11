// Build a tiny WASM caller that drives AnyWasm's real emitted-module import with unique IDs.
// This isolates the host-side cache from PE decoding while exercising compile, eviction, reuse,
// and the generated-module trampoline in Node and browser engines.
function uleb(value) {
  let n = BigInt(value);
  const bytes = [];
  do {
    let byte = Number(n & 0x7fn);
    n >>= 7n;
    if (n !== 0n) byte |= 0x80;
    bytes.push(byte);
  } while (n !== 0n);
  return bytes;
}

function sleb(value) {
  let n = BigInt(value);
  const bytes = [];
  let more;
  do {
    let byte = Number(n & 0x7fn);
    n >>= 7n;
    const sign = (byte & 0x40) !== 0;
    more = !((n === 0n && !sign) || (n === -1n && sign));
    if (more) byte |= 0x80;
    bytes.push(byte);
  } while (more);
  return bytes;
}

const section = (id, payload) => [id, ...uleb(payload.length), ...payload];
const name = (text) => {
  const bytes = [...new TextEncoder().encode(text)];
  return [...uleb(bytes.length), ...bytes];
};

function emittedModule() {
  const bytes = [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
  bytes.push(...section(1, [1, 0x60, 2, 0x7f, 0x7f, 1, 0x7f]));
  bytes.push(...section(2, [
    1, ...name('anywasm'), ...name('memory'), 2, 0, 1,
  ]));
  bytes.push(...section(3, [1, 0]));
  bytes.push(...section(7, [1, ...name('run'), 0, 0]));
  bytes.push(...section(10, [1, 4, 0, 0x41, 1, 0x0b]));
  return bytes;
}

function runBody(ids, moduleLength) {
  const body = [0]; // no locals
  for (const id of ids) {
    for (const value of [id, 1024, moduleLength, 2048, 1, 1]) body.push(0x41, ...sleb(value));
    body.push(0x41, 0x7f); // firstRegisterRegister = u32::MAX
    body.push(0x10, 0, 0x1a); // call import 0, drop result
  }
  body.push(0x41, 0, 0x0b); // return 0
  return body;
}

function callerModule(pressureIds, probeIds, codeBytes) {
  const bytes = [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
  bytes.push(...section(1, [
    3,
    0x60, 7, ...Array(7).fill(0x7f), 1, 0x7f, // run_emitted_block: (7 x i32) -> i32
    0x60, 1, 0x7e, 1, 0x7f, // aw_run: (i64) -> i32
    0x60, 0, 1, 0x7f, // aw_probe: () -> i32
  ]));
  bytes.push(...section(2, [
    1,
    ...name('anywasm'), ...name('run_emitted_block'), 0, 0,
  ]));
  bytes.push(...section(3, [2, 1, 2]));
  bytes.push(...section(5, [1, 0, 1])); // one-page memory, exported for emitted modules
  bytes.push(...section(7, [
    3,
    ...name('memory'), 2, 0,
    ...name('aw_run'), 0, 1,
    ...name('aw_probe'), 0, 2,
  ]));

  const pressureBody = runBody(pressureIds, codeBytes.length);
  const probeBody = runBody(probeIds, codeBytes.length);
  const code = [2, ...uleb(pressureBody.length), ...pressureBody, ...uleb(probeBody.length), ...probeBody];
  bytes.push(...section(10, code));

  const data = [1, 0, 0x41, ...sleb(1024), 0x0b, ...uleb(codeBytes.length), ...codeBytes];
  bytes.push(...section(11, data));
  return new Uint8Array(bytes);
}

/** Run 300 distinct emitted modules, revisit the first 44, then probe all IDs again. */
export async function runJitCachePressure(AnyWasm) {
  const pressureIds = [...Array.from({ length: 300 }, (_, index) => index + 1), ...Array.from({ length: 44 }, (_, index) => index + 1)];
  const probeIds = [...Array.from({ length: 44 }, (_, index) => index + 1), 45, 300];
  const generated = emittedModule();
  const runtime = await AnyWasm.create(callerModule(pressureIds, probeIds, generated), { jitMinPrefix: 1 });
  const firstStatus = runtime.run(0);
  const afterPressure = { ...runtime.jitStats };
  const probeStatus = runtime.x.aw_probe();
  return { firstStatus, afterPressure, probeStatus, afterProbe: { ...runtime.jitStats } };
}
