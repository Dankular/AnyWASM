#!/usr/bin/env bash
# Build the full AnyWASM runtime against an imported *shared* WebAssembly.Memory, so CPU workers
# can address guest frames in place (no snapshot copies). Output: web/anywasm_wasm_shared.wasm.
# The prebuilt std lacks atomics metadata, hence --no-check-features; the runtime is only ever
# driven from one thread (the coordinator), while workers touch guest frames via atomic accesses.
set -euo pipefail
cd "$(dirname "$0")/.."
RELEASE_DIR=target/shared/wasm32-unknown-unknown/release
rustflags='["-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals","-Clink-arg=--import-memory","-Clink-arg=--shared-memory","-Clink-arg=--no-check-features","-Clink-arg=--global-base=1179648","-Clink-arg=--initial-memory=268435456","-Clink-arg=--max-memory=2147483648","-Clink-arg=--export=__stack_pointer","-Clink-arg=--export=__heap_base"]'
CARGO_TARGET_DIR=target/shared cargo build -p anywasm-wasm --target wasm32-unknown-unknown --release --config "build.rustflags=$rustflags"
node scripts/asyncify-wasm.mjs "$RELEASE_DIR/anywasm_wasm.wasm" "$RELEASE_DIR/anywasm_wasm.asyncified.wasm"
cp "$RELEASE_DIR/anywasm_wasm.asyncified.wasm" web/anywasm_wasm_shared.wasm
ls -l web/anywasm_wasm_shared.wasm

# CPU worker for the in-place layout. The runtime above leaves [0x100000, 0x120000) unused: the
# pool puts its ABI header and worker slots at 0x100000 and this worker's static data at 0x104000.
worker_flags='["-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals","-Clink-arg=--import-memory","-Clink-arg=--shared-memory","-Clink-arg=--no-check-features","-Clink-arg=--global-base=1064960","-Clink-arg=--initial-memory=268435456","-Clink-arg=--max-memory=2147483648","-Clink-arg=--export=__stack_pointer","-Clink-arg=--export=__heap_base","-Clink-arg=--export=__data_end"]'
CARGO_TARGET_DIR=target/shared-worker cargo build -p anywasm-cpu-worker --target wasm32-unknown-unknown --release --config "build.rustflags=$worker_flags"
cp target/shared-worker/wasm32-unknown-unknown/release/anywasm_cpu_worker.wasm web/anywasm_cpu_worker_inplace.wasm
ls -l web/anywasm_cpu_worker_inplace.wasm
