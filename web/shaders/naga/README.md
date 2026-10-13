# SPIR-V to WGSL WebAssembly bridge

This directory contains the pinned Naga 30.0.1 SPIR-V-to-WGSL bridge used by
`web/shaders.js`. The generated module parses and validates SPIR-V, emits WGSL,
and validates the emitted WGSL. The wrapper exposes
`spirv_to_wgsl(Uint8Array): string` and keeps the existing group 3 draw-parameter
uniform at binding 0.

Naga's SPIR-V frontend does not model array layers in `OpImageQuerySize`. The
retained Naga source includes a local fix that composes the spatial size query
with `ImageQuery::NumLayers`, so `Texture1DArray` and `Texture2DArray`
`GetDimensions` calls can be translated. The wrapper enables Naga's
`CUBE_ARRAY_TEXTURES` validation capability, allowing `TextureCubeArray`
`SampleLevel` and `GetDimensions` to lower to WGSL `texture_cube_array` types.
The browser regression checks a two-cube SRV range, cube-face filtering, and
the selected view's dimensions. The 1D D3D texture lowering in
`web/shaders.js` maps its backing image query to the one-dimensional width and
array-element outputs.

To rebuild on Windows, install the Rust `wasm32-unknown-unknown` target and
`wasm-bindgen` 0.2.121, then run `./build.ps1`. The script uses the committed
lockfile and source crates and updates the generated JavaScript, WebAssembly,
and manifest hashes.
