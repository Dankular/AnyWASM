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
- Cached-block execution benchmark (Node 24.16.0, Windows x64, release WASM; two runs, six rounds
  per workload): median 32.2 M instr/s for `compute.exe`, 26.1 for call-heavy, 28.1 for
  memory-heavy, and 38.5 for SSE-float. The cached executor translates NOP-like, MOV and LEA
  operations; other instructions use the interpreter. These figures predate dynamic WASM emission
  and are not performance measurements of the emitted path. The benchmark output is retained per
  commit by WASM CI.
- Dynamic-emitter gate sweep (`node test/bench.mjs`, Node 24.16.0/Windows x64, six rounds per
  workload) compared minimum supported MOV-prefix lengths 1, 2, 3, 4, and 65 (cached-only). Median
  M instr/s by workload were:

  | Minimum prefix | Compute | Call-heavy | Memory-heavy | SSE-float | Emitted calls |
  | ---: | ---: | ---: | ---: | ---: | --- |
  | 1 | 14.4 | 15.35 | 13.25 | 32.5 | 343,561 / 500,000 / 262,016 / 0 |
  | 2 | 29.8 | 25.05 | 26.55 | 31.65 | 2 / 0 / 0 / 0 |
  | 3 | 29.6 | 25.1 | 26.85 | 31.35 | 2 / 0 / 0 / 0 |
  | 4 | 29.95 | 25.25 | 27.5 | 37.9 | 0 / 0 / 0 / 0 |
  | 65 (default) | 30.0 | 25.55 | 26.55 | 38.8 | 0 / 0 / 0 / 0 |

  Prefix 1 reproduces the large regressions seen in the initial emitter sample. Prefixes 2 and 3
  emit only two compute calls in these fixtures and do not consistently outperform cached-only;
  prefix 4 emits nothing and still scans candidate prefixes. Since no conservative threshold shows
  a repeatable benefit, dynamic emission is disabled by default with threshold 65, above the
  64-instruction block limit. Set `{ jitMinPrefix: 1 }` to force the prototype for experiments and
  correctness checks. These are local benchmark samples, not a speedup claim. The earlier
  cached-block baseline above was collected separately (two runs); compare like-for-like only in
  future controlled runs.
- CI (GitHub Actions, user-provided workflow): `cargo build` + `cargo test` pass, including the
  hardware-differential suite, on the hosted runner.

## Known limits (not hidden)
- x87 is modelled with f64, not 80-bit; transcendentals compared to 1e-9 relative tolerance.
- rcpps/rsqrtps return exact reciprocals (excluded from the diff corpus).
- Not decoded: VEX/EVEX (AVX, BMI), MMX, AES/PCLMUL, 16-bit/segment modes. CPUID hides them.
- Diff corpus covers single instructions and x87 sequences; control flow, push/pop and syscall are
  covered only by the end-to-end programs.
- **Dynamic WASM emission (opt-in prototype, default disabled):** at a block entry, Rust emits a module for a
  leading sequence of 32-bit/64-bit GPR-immediate and GPR-to-GPR MOVs. Register-to-register 32-bit
  moves zero-extend; self-moves retire normally. The module imports the main runtime's
  memory and runs through `anywasm.run_emitted_block` in `web/anywasm.js`. It mutates a copied
  160-byte CPU snapshot (GPRs, RIP, RFLAGS, retired count, TSC); Rust commits only a valid exact
  instruction count. Compile/trap/invalid-count failures retry through the interpreter. The JS
  wrapper caches up to 256 generated modules and validates bytes if an ID is reused. Rust tests,
  `test/wasm_smoke.mjs` (including explicitly forced generated execution, one-instruction budgets,
  and same-bundle comparisons against both the default cached-only path and `{ jit: false }` for
  compute status, exit code, stdout/stderr, and retired count) pass. The smoke also confirms the
  default gate skips the fixture's short prefixes and forces one host module-compile failure with
  safe interpreter fallback. The headless Chrome suite also runs `compute.exe` through the opt-in
  emitter in the page realm, confirms emitted register-register MOV execution without fallback, and
  compares output and retired count with cached-only execution. The standard worker, input, and
  WebGPU checks pass with the default gate. Final register/memory comparisons remain open. This is a
  mechanism prototype, not a throughput win: emission requires explicit opt-in; partial-width and
  memory MOVs, NOP, LEA, arithmetic,
  SIMD, or control flow are not emitted. Full JIT/interpreter state differential, module-cache
  pressure, CSP checks, forced-emitter worker execution, browser
  engine matrix, and real-game performance remain open.
- No threads (CreateThread/_beginthreadex report "unimplemented"), no SEH/C++ exception unwinding,
  no GDI, no audio, no DirectInput/XInput, no 32-bit PE, no D3D9/D3D12/OpenGL.
- Direct3D 11 coverage is a vertical slice: textures, depth/stencil, blending, rasterizer and
  sampler state, mip generation, copies and instancing are implemented in the translator, but only two
  paths are pixel-verified: the coloured triangle (216 pixels) and `d3d11_textured` (2x2 point-sampled
  texture, D32_FLOAT depth test with the near quad drawn first, 16-bit indexed draws, 3 quads).
  Blending, alpha-test, mips/filtering, instancing and copies are not pixel-verified. Compute shaders/UAVs, geometry/tessellation, multisampling
  (forced to 1 sample), staging read-back of GPU-written data, and `SV_` system values beyond the
  basics are not implemented.
- **Test-environment limits (not product limits):** in the headless Chromium used here,
  `getCurrentTexture()` makes later `GPUBuffer.mapAsync` fail and screenshots do not capture WebGPU
  canvases (both reproduced in a 10-line standalone page). So pixel verification runs in a test mode
  that skips the final canvas blit (`?test=1`); **the blit to the visible canvas is built and passes
  WebGPU validation but its pixels have not been verified here.**
- Browser input (keyboard/mouse -> window messages) and the idle/frame scheduling in the worker are
  exercised by the Chromium browser suite. Firefox/Safari, visible canvas pixel output, large-game
  memory limits, and asset streaming remain unverified; see
  [`BROWSER_SUPPORT.md`](BROWSER_SUPPORT.md).
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.

## Remaining target-level and project work
- The section 1–6 checklist in `CLAUDE.md` tracks unfinished JIT, runtime, graphics, CPU, and
  productisation work. Broad items such as threading, SEH, more D3D11 coverage, AVX, and 32-bit PE
  support need a representative target and dedicated design/verification before implementation.
- Three Diligent Win64 samples and the selected Diligent GLFW Demo maze have been built and analyzed.
  A D3D11-only `/MT` build reduced the maze package's missing static imports from 349 to 122, but a
  bounded AnyWASM run now passes CRT `MultiByteToWideChar` initialization and stops at a null write
  in `LCMapStringEx` before window creation. The former has a guest regression covering the Win32
  capacity contract and signed length parameters. No real-game acceptance pass is claimed. See
  [`docs/TARGETS.md`](TARGETS.md) for exact traces and next steps.
- The D3D11 textured fixture now has Rust command-stream and WASM smoke coverage. Its WebGPU pixel
  checks remain in `test/browser/run.mjs`; other rendering states listed in `CLAUDE.md` are not
  pixel-verified.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
