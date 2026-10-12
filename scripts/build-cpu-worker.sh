#!/usr/bin/env bash
# Build the standalone no-std worker module against an imported shared Wasm memory.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p anywasm-cpu-worker --target wasm32-unknown-unknown --release \
  --config 'build.rustflags=["-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals","-Clink-arg=--import-memory","-Clink-arg=--shared-memory","-Clink-arg=--export=__stack_pointer","-Clink-arg=--no-check-features","-Clink-arg=--initial-memory=33554432","-Clink-arg=--max-memory=1073741824"]'

printf 'Built %s\n' "target/wasm32-unknown-unknown/release/anywasm_cpu_worker.wasm"
