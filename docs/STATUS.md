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
- First emitted-path sample (`node test/bench.mjs`, Node 24.16.0/Windows x64, one run with three
  rounds): medians were 22.9 M instr/s compute, 15.0 call-heavy, 25.5 memory-heavy, and 38.5
  SSE-float. The benchmark now prints emitted-call/module/fallback counts: compute exercised 60,007
  emitted calls across 16 modules; call-heavy exercised 500,000 calls from one module; memory/SSE
  did not enter the supported subset. This is not an apples-to-apples comparison with the earlier
  six-round/two-run baseline. It shows no demonstrated speedup yet; the frequent host trampoline
  cost is a likely contributor and needs isolated measurement or batching.
- CI (GitHub Actions, user-provided workflow): `cargo build` + `cargo test` pass, including the
  hardware-differential suite, on the hosted runner.

## Known limits (not hidden)
- x87 is modelled with f64, not 80-bit; transcendentals compared to 1e-9 relative tolerance.
- rcpps/rsqrtps return exact reciprocals (excluded from the diff corpus).
- Not decoded: VEX/EVEX (AVX, BMI), MMX, AES/PCLMUL, 16-bit/segment modes. CPUID hides them.
- Diff corpus covers single instructions and x87 sequences; control flow, push/pop and syscall are
  covered only by the end-to-end programs.
- **Dynamic WASM emission (first verified slice):** at a block entry, Rust emits a module for a
  leading sequence of 32-bit/64-bit register-immediate MOVs. The module imports the main runtime's
  memory and runs through `anywasm.run_emitted_block` in `web/anywasm.js`. It mutates a copied
  160-byte CPU snapshot (GPRs, RIP, RFLAGS, retired count, TSC); Rust commits only a valid exact
  instruction count. Compile/trap/invalid-count failures retry through the interpreter. The JS
  wrapper caches up to 256 generated modules and validates bytes if an ID is reused. Rust tests,
  `test/wasm_smoke.mjs` (including generated execution and one-instruction budgets), and the
  headless Chrome worker suite pass. This is a mechanism prototype, not a throughput win: no
  register-register/partial-width MOV, NOP, LEA, memory operations, arithmetic, SIMD, or control
  flow is emitted. Full JIT/interpreter state differential, cache-pressure and CSP checks, browser
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
  implemented but only exercised indirectly; there is no automated interaction test yet.
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.

## Remaining target-level and project work
- The section 1–6 checklist in `CLAUDE.md` tracks unfinished JIT, runtime, graphics, CPU, and
  productisation work. Broad items such as threading, SEH, more D3D11 coverage, AVX, and 32-bit PE
  support need a representative target and dedicated design/verification before implementation.
- Three Diligent Win64 sample executables have been built and analyzed; each still has 100–116
  unresolved imports. They are preliminary analysis targets, not run-ready acceptance targets. See
  [`docs/TARGETS.md`](TARGETS.md) for pinned source revisions, build notes, import groups, and
  redistribution caveats.
- The D3D11 textured fixture now has Rust command-stream and WASM smoke coverage. Its WebGPU pixel
  checks remain in `test/browser/run.mjs`; other rendering states listed in `CLAUDE.md` are not
  pixel-verified.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
