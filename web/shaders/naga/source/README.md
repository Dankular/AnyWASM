# Naga bridge source

This is the source crate for the generated bridge in the parent directory. It
uses Naga 30.0.1 with the array image-size query fix in
`naga/src/front/spv/image.rs` and enables `CUBE_ARRAY_TEXTURES` validation for
cube-array shaders. `naga` and its matching `naga-types` crate are retained
locally so the bridge can be rebuilt without changing the pinned upstream
versions.

Build from the repository root with `web/shaders/naga/build.ps1`. The build
requires Cargo, the `wasm32-unknown-unknown` Rust target, and wasm-bindgen
0.2.121.
