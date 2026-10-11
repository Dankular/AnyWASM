import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { AnyWasm, Status } from '../web/anywasm.js';

const STATUS_NAMES = new Map(Object.entries(Status).map(([name, value]) => [value, name]));

function positiveSafeInteger(name, value) {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
  return value;
}

function terminalState(wasm, status) {
  return {
    status,
    name: STATUS_NAMES.get(status) ?? `UNKNOWN_${status}`,
    exitCode: wasm.exitCode(),
    message: wasm.message(),
  };
}

/**
 * Run an .awb bundle to frame boundaries, bounded by a guest-instruction cap.
 * Times are measured after bundle loading; the first frame includes startup.
 */
export async function measureFrameCosts(wasmBytes, bundleBytes, {
  frameCount = 8,
  maxGuestInstructions = 1_000_000_000,
  sliceInstructions = 10_000_000,
  jitMinPrefix = Number(process.env.ANYWASM_JIT_MIN_PREFIX ?? 65),
} = {}) {
  positiveSafeInteger('frameCount', frameCount);
  positiveSafeInteger('maxGuestInstructions', maxGuestInstructions);
  positiveSafeInteger('sliceInstructions', sliceInstructions);
  const wasm = await AnyWasm.create(wasmBytes, { jitMinPrefix });
  wasm.load(bundleBytes);

  const startedAt = performance.now();
  let lastFrameAt = startedAt;
  const initialInstructions = wasm.retiredInstructions();
  let previousInstructions = initialInstructions;
  const frames = [];
  let terminal = null;

  while (frames.length < frameCount) {
    const totalBeforeRun = wasm.retiredInstructions();
    const consumed = totalBeforeRun - initialInstructions;
    const remaining = maxGuestInstructions - consumed;
    if (remaining <= 0) break;

    const status = wasm.run(Math.min(sliceInstructions, remaining));
    const now = performance.now();
    const currentInstructions = wasm.retiredInstructions();
    const frameInstructions = currentInstructions - previousInstructions;

    if (status === Status.FRAME) {
      frames.push({
        ordinal: frames.length + 1,
        guestInstructions: frameInstructions,
        milliseconds: now - lastFrameAt,
      });
      previousInstructions = currentInstructions;
      lastFrameAt = now;
      // Copy the command stream out of WASM memory before clearing its buffer.
      wasm.takeGpu();
      continue;
    }

    if (status !== Status.RUNNING) {
      terminal = terminalState(wasm, status);
      break;
    }

    if (currentInstructions === totalBeforeRun) {
      throw new Error(`runtime made no guest-instruction progress (status ${status})`);
    }
  }

  const totalGuestInstructions = wasm.retiredInstructions();
  const elapsedMilliseconds = performance.now() - startedAt;
  const limitReached = frames.length < frameCount && totalGuestInstructions >= maxGuestInstructions;
  return {
    requestedFrames: frameCount,
    frames,
    terminal,
    limitReached,
    totalGuestInstructions,
    elapsedMilliseconds,
    jitStats: { ...wasm.jitStats },
  };
}

function parsePositiveOption(name, value) {
  if (!/^\d+$/.test(value)) throw new Error(`${name} expects a positive integer`);
  return positiveSafeInteger(name, Number(value));
}

function parseArgs(args) {
  const options = {};
  let bundlePath;
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--help' || arg === '-h') return { help: true };
    if (arg.startsWith('--')) {
      const values = {
        '--frames': 'frameCount',
        '--max-instructions': 'maxGuestInstructions',
        '--slice-instructions': 'sliceInstructions',
        '--jit-min-prefix': 'jitMinPrefix',
      };
      const key = values[arg];
      if (!key) throw new Error(`unknown option: ${arg}`);
      const value = args[++i];
      if (value === undefined) throw new Error(`${arg} requires a value`);
      options[key] = parsePositiveOption(arg, value);
      continue;
    }
    if (bundlePath !== undefined) throw new Error('only one .awb bundle path may be provided');
    bundlePath = arg;
  }
  if (!bundlePath) throw new Error('usage: node test/target_frame_bench.mjs <bundle.awb> [options]');
  return { bundlePath, options };
}

const HELP = `Usage: node test/target_frame_bench.mjs <bundle.awb> [options]

Options:
  --frames N              Frame boundaries to collect (default: 8)
  --max-instructions N    Guest instruction cap (default: 1000000000)
  --slice-instructions N  Instructions per runtime call (default: 10000000)
  --jit-min-prefix N      MOV prefix threshold (default: ANYWASM_JIT_MIN_PREFIX or 65)
`;

async function main() {
  const { bundlePath, options, help } = parseArgs(process.argv.slice(2));
  if (help) {
    process.stdout.write(HELP);
    return;
  }
  const repoRoot = fileURLToPath(new URL('..', import.meta.url));
  const wasmPath = resolve(repoRoot, 'target/wasm32-unknown-unknown/release/anywasm_wasm.wasm');
  const result = await measureFrameCosts(readFileSync(wasmPath), readFileSync(resolve(bundlePath)), options);
  process.stdout.write(`${JSON.stringify({ bundle: resolve(bundlePath), wasm: wasmPath, ...result }, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error}\n`);
    process.exitCode = 2;
  });
}
