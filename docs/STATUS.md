# Status (work in progress)

## Verified (tests pass; run `cargo test`)
- `anywasm-pe`: PE32/PE32+ parser. 9 tests, values cross-checked against `llvm-readobj` on
  clang/lld-link-built fixtures (`fixtures/build.sh`).
- `anywasm-cpu`: paged memory, decoder, integer/SSE..SSE4.2/x87 interpreter.
  **Differential test against the host CPU** (`tests/diff.rs`): 2,730 llvm-mc-assembled corpus
  entries x 24 random trials, comparing GPRs, flags, XMM and memory. It has found and fixed 8 real
  defects. It passed on this sandbox's Intel Xeon and on a GitHub-hosted runner's CPU, except for
  one case: 16-bit `shld/shrd` with count > 16, which the SDM calls undefined and vendors differ
  on (the first CI run caught this; that range is now excluded from the corpus and the interpreter
  keeps the Intel behaviour, documented in `exec.rs`).
- `anywasm-rt`: PE64 loader (relocations, imports, delay imports, TLS, TEB/PEB with loader lists,
  bundled DLLs) + host-implemented kernel32/ntdll/CRT. 5 end-to-end tests run **real Windows x64
  executables**: freestanding clang-built programs (console I/O, relocated pointer tables, heap,
  SSE2 math, indirect calls, GetProcAddress) and a mingw-w64 program importing the real
  `msvcrt.dll` (argv, qsort with guest callbacks, printf family, malloc/realloc, math,
  setjmp/longjmp, file I/O on the VFS, sscanf, atexit, exit code, stderr). The mingw program's
  output is identical to the same source compiled natively (checked by hand).
- `printf` engine: unit tests for flags/width/precision/length modifiers/floats.
- `anywasm` CLI (`analyze`, `run`, `pack`) and `.awb` bundles; the same executables run through the
  **WebAssembly build** under Node (`test/wasm_smoke.mjs`) with output identical to native, and in
  **headless Chromium** through the real page and worker (`test/browser/run.mjs`).
- **Direct3D 11 -> WebGPU, end to end.** A mingw-compiled Win64 program using the real `d3d11.h`
  COM API (window class + `CreateWindowEx`, `D3D11CreateDeviceAndSwapChain`, buffers, DXBC shaders,
  input layout, `Map`/`Unmap`, `Present`) runs in the interpreter; COM vtables are generated from the
  mingw headers; calls become a binary GPU command stream (`tests/d3d11_stream.rs` checks it);
  `web/gpu.js` replays it with WebGPU through DXBC -> SPIR-V -> WGSL (vendored vkd3d-shader + Naga).
  The back buffer is read back and compared with analytic barycentric colours: 216 sampled pixels,
  worst error 0/255, zero WebGPU validation errors.
- Shader toolchain (`test/shaders.mjs`): HLSL -> DXBC -> SPIR-V -> WGSL with a fixed
  descriptor-to-bind-group layout (b#->group0, t#->group1, s#->group2).
- CI (GitHub Actions, user-provided workflow): `cargo build` + `cargo test` pass, including the
  hardware-differential suite, on the hosted runner.

## Known limits (not hidden)
- x87 is modelled with f64, not 80-bit; transcendentals compared to 1e-9 relative tolerance.
- rcpps/rsqrtps return exact reciprocals (excluded from the diff corpus).
- Not decoded: VEX/EVEX (AVX, BMI), MMX, AES/PCLMUL, 16-bit/segment modes. CPUID hides them.
- Diff corpus covers single instructions and x87 sequences; control flow, push/pop and syscall are
  covered only by the end-to-end programs.
- **Performance:** interpreter with a decode cache only (no JIT). Measured ~19 M guest instr/s in
  WASM under Node, ~3x better than before the cache. Real games need 10-50x more: a block-translating
  JIT to WASM is the main missing piece for playable speed.
- No threads (CreateThread/_beginthreadex report "unimplemented"), no SEH/C++ exception unwinding,
  no GDI, no audio, no DirectInput/XInput, no 32-bit PE, no D3D9/D3D12/OpenGL.
- Direct3D 11 coverage is a vertical slice: textures, depth/stencil, blending, rasterizer and
  sampler state, mip generation, copies and instancing are implemented in the translator but only the
  coloured-triangle path is pixel-verified. Compute shaders/UAVs, geometry/tessellation, multisampling
  (forced to 1 sample), staging read-back of GPU-written data, and `SV_` system values beyond the
  basics are not implemented.
- **Test-environment limits (not product limits):** in the headless Chromium used here,
  `getCurrentTexture()` makes later `GPUBuffer.mapAsync` fail and screenshots do not capture WebGPU
  canvases (both reproduced in a 10-line standalone page). So pixel verification runs in a test mode
  that skips the final canvas blit (`?test=1`); **the blit to the visible canvas is built and passes
  WebGPU validation but its pixels have not been verified here.**
- Browser input (keyboard/mouse -> window messages) and the idle/frame scheduling in the worker are
  implemented but only exercised indirectly; there is no automated interaction test yet.
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.

## Not started
- anywasm CLI (analyze/pack), WASM crate + browser harness, user32/gdi32, D3D/WebGPU, threads, SEH.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
