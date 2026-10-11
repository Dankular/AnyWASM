#!/usr/bin/env bash
# Build the browser bundle: wasm module + demo bundles into web/.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p anywasm-wasm --target wasm32-unknown-unknown --release
cargo build -p anywasm-cli
cp target/wasm32-unknown-unknown/release/anywasm_wasm.wasm web/anywasm_wasm.wasm
mkdir -p web/demo
./target/debug/anywasm pack fixtures/hello.exe -o web/demo/hello.awb
./target/debug/anywasm pack fixtures/crt_test.exe -o web/demo/crt_test.awb --args '"C:\crt_test.exe" first "second arg"'
ls -l web/anywasm_wasm.wasm web/demo/
