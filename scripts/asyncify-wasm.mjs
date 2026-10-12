import binaryen from 'binaryen';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const inputPath = resolve(process.argv[2] || resolve(repoRoot, 'target/wasm32-unknown-unknown/release/anywasm_wasm.wasm'));
const outputPath = resolve(process.argv[3] || resolve(repoRoot, 'target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm'));
const inputBytes = readFileSync(inputPath);
const module = binaryen.readBinary(inputBytes);

try {
  binaryen.setPassArgument('asyncify-imports', 'anywasm.yield_slice,anywasm.read_asset_range,anywasm.gpu_readback');
  module.runPasses(['asyncify']);
  binaryen.setPassArgument('asyncify-imports', null);

  if (!module.validate()) throw new Error('Binaryen produced an invalid Asyncify module');
  const outputBytes = module.emitBinary();
  const wasmModule = new WebAssembly.Module(outputBytes);
  const exports = new Set(WebAssembly.Module.exports(wasmModule).map((entry) => entry.name));
  const imports = WebAssembly.Module.imports(wasmModule);
  const requiredExports = [
    'asyncify_start_unwind',
    'asyncify_stop_unwind',
    'asyncify_start_rewind',
    'asyncify_stop_rewind',
    'asyncify_get_state',
  ];
  const missing = requiredExports.filter((name) => !exports.has(name));
  if (missing.length) throw new Error(`Asyncify exports are missing: ${missing.join(', ')}`);
  if (!imports.some((entry) => entry.module === 'anywasm' && entry.name === 'yield_slice')) {
    throw new Error('WASM module does not import anywasm.yield_slice');
  }
  if (!imports.some((entry) => entry.module === 'anywasm' && entry.name === 'read_asset_range')) {
    throw new Error('WASM module does not import anywasm.read_asset_range');
  }
  if (!imports.some((entry) => entry.module === 'anywasm' && entry.name === 'gpu_readback')) {
    throw new Error('WASM module does not import anywasm.gpu_readback');
  }

  writeFileSync(outputPath, outputBytes);
  console.log(`Asyncified ${inputBytes.length} bytes to ${outputBytes.length} bytes: ${outputPath}`);
} finally {
  binaryen.setPassArgument('asyncify-imports', null);
  module.dispose();
}
