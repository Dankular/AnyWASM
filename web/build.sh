#!/usr/bin/env bash
# Build the browser bundle: wasm module + demo bundles into web/.
set -euo pipefail
cd "$(dirname "$0")/.."
npm ci
cargo build -p anywasm-wasm --target wasm32-unknown-unknown --release
node scripts/asyncify-wasm.mjs
cargo build -p anywasm-cli
rustflags='["-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals","-Clink-arg=--import-memory","-Clink-arg=--shared-memory","-Clink-arg=--no-check-features","-Clink-arg=--initial-memory=33554432","-Clink-arg=--max-memory=1073741824","-Clink-arg=--export=__stack_pointer","-Clink-arg=--export=__heap_base"]'
cargo build -p anywasm-cpu-worker --target wasm32-unknown-unknown --release --config "build.rustflags=$rustflags"
cp target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm web/anywasm_wasm.wasm
cp target/wasm32-unknown-unknown/release/anywasm_cpu_worker.wasm web/anywasm_cpu_worker.wasm
mkdir -p web/demo
./target/debug/anywasm pack fixtures/hello.exe -o web/demo/hello.awb
./target/debug/anywasm pack fixtures/crt_test.exe -o web/demo/crt_test.awb --args '"C:\crt_test.exe" first "second arg"'
./target/debug/anywasm pack fixtures/d3d11_triangle.exe -o web/demo/d3d11_triangle.awb
./target/debug/anywasm pack fixtures/d3d11_textured.exe -o web/demo/d3d11_textured.awb
./target/debug/anywasm pack fixtures/d3d11_texload.exe -o web/demo/d3d11_texload.awb
./target/debug/anywasm pack fixtures/parallel_test.exe -o web/demo/parallel_test.awb
./target/debug/anywasm pack fixtures/input_test.exe -o web/demo/input_test.awb
ls -l web/anywasm_wasm.wasm web/demo/
