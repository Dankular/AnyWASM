#!/usr/bin/env bash
# Build the browser bundle: wasm module + demo bundles into web/.
set -euo pipefail
cd "$(dirname "$0")/.."
npm ci
cargo build -p anywasm-wasm --target wasm32-unknown-unknown --release
node scripts/asyncify-wasm.mjs
cargo build -p anywasm-cli
cp target/wasm32-unknown-unknown/release/anywasm_wasm.asyncified.wasm web/anywasm_wasm.wasm
mkdir -p web/demo
./target/debug/anywasm pack fixtures/hello.exe -o web/demo/hello.awb
./target/debug/anywasm pack fixtures/crt_test.exe -o web/demo/crt_test.awb --args '"C:\crt_test.exe" first "second arg"'
./target/debug/anywasm pack fixtures/d3d11_triangle.exe -o web/demo/d3d11_triangle.awb
./target/debug/anywasm pack fixtures/d3d11_textured.exe -o web/demo/d3d11_textured.awb
./target/debug/anywasm pack fixtures/input_test.exe -o web/demo/input_test.awb
ls -l web/anywasm_wasm.wasm web/demo/
