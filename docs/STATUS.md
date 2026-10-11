# Status (work in progress)

## Verified (tests pass; run `cargo test`)
- `anywasm-pe`: PE32/PE32+ parser. 11 tests, values cross-checked against `llvm-readobj` on
  clang/lld-link-built fixtures (`fixtures/build.sh`).
- `anywasm-cpu`: paged memory, decoder, integer/SSE..SSE4.2/x87 interpreter.
  **Differential test against the host CPU** (`tests/diff.rs`): 2,730 llvm-mc-assembled corpus
  entries x 24 random trials, comparing GPRs, flags, XMM and memory. It has found and fixed 8 real
  defects. It passed on this sandbox's Intel Xeon and on a GitHub-hosted runner's CPU, except for
  one case: 16-bit `shld/shrd` with count > 16, which the SDM calls undefined and vendors differ
  on (the first CI run caught this; that range is now excluded from the corpus and the interpreter
  keeps the Intel behaviour, documented in `exec.rs`).
- `anywasm-rt`: PE64 loader (relocations, imports, delay imports, validated `.pdata`, per-thread static TLS, TEB/PEB with loader lists,
  bundled DLLs) + host-implemented kernel32/ntdll/CRT. 31 end-to-end tests run **real Windows x64
  executables**: freestanding fixtures cover console I/O, relocated pointer tables, heap, SSE2 math,
  indirect calls, GetProcAddress, thread synchronization, static TLS isolation, version checks, and
  VFS paths; a mingw-w64 program imports the real `msvcrt.dll` (argv, qsort with guest callbacks,
  printf family, malloc/realloc, math, setjmp/longjmp, file I/O on the VFS, sscanf, atexit, exit
  code, stderr). The mingw program's output matches the same source compiled natively.
- `printf` engine: unit tests for flags/width/precision/length modifiers/floats.
- `anywasm` CLI (`analyze`, `run`, `pack`) and `.awb` bundles. `run` and `pack` support `--cwd`,
  bundles retain that guest directory, and the WASM loader restores it; relative Win32 and CRT file
  paths resolve against process state. Old bundles default to `C:\`. The same executables run through
  the **WebAssembly build** under Node (`test/wasm_smoke.mjs`) with output identical to native, and in
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
- Representative-target guest benchmark (`test/target_frame_bench.mjs`): Diligent GLFW Demo,
  release WASM under Node 24.16.0/Windows x64, eight frame stops and a 1,000,000,000-instruction
  cap. Cached-only retired 576.57M process-wide guest instructions before its first stop in 42.28 s;
  prefix 1 took 69.14 s with 687,352 module compilations and 13.09M emitted-block executions.
  Both runs then retired 3,199 instructions per frame in under 0.6 ms, reached all eight guest
  `Present` stops, and reported no terminal state or cap. These are emulator guest/HLE measurements;
  the target command stream was not sent to WebGPU and no host pixels or game performance were
  measured. Full per-frame data and commands are in `docs/TARGETS.md`.
- CI (GitHub Actions, user-provided workflow): `cargo build` + `cargo test` pass, including the
  hardware-differential suite, on the hosted runner.

## Known limits (not hidden)
- x87 is modelled with f64, not 80-bit; transcendentals compared to 1e-9 relative tolerance.
- rcpps/rsqrtps return exact reciprocals (excluded from the diff corpus).
- Selected VEX instructions are decoded for target-used scalar/vector moves and arithmetic, XOR,
  rounding/conversion, `VEXTRACTF128`, `VZEROUPPER`, `ANDN`, `SHLX`, `SHRX`, and `BZHI` (see the
  exact list in `CLAUDE.md`). This is not broad AVX/AVX2/BMI coverage; EVEX, FMA, MMX, AES/PCLMUL,
  and 16-bit/segment modes remain open.
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
  compares output and retired count with cached-only execution. It also grows WASM memory by one
  page after an emitted block has run, then verifies further emitted executions complete without
  fallback and preserve output/count parity. A dedicated browser-worker run forces the emitter,
  confirms register-register execution without fallback, and matches the page-realm output and
  retired count. The standard worker, input, and WebGPU checks pass with the default gate. Final guest
  register/memory comparisons remain open. This is a
  mechanism prototype, not a throughput win: emission requires explicit opt-in; partial-width and
  memory MOVs, NOP, LEA, arithmetic,
  SIMD, or control flow are not emitted. Full JIT/interpreter state differential, module-cache
  pressure, CSP checks, browser engine matrix, and real-game performance remain open.
- Thread support uses a deterministic cooperative scheduler with saved CPU/stack/TEB TLS pointers,
  dynamic TLS/FLS maps, static PE TLS blocks, quantum rotation, resumable waits, events, mutexes,
  semaphores, critical sections, SRW locks, and condition-variable wake/reacquire behavior. Nested
  synchronous callback scheduling and alertable waits remain unsupported. No SEH/C++ exception
  unwinding, audio, DirectInput/XInput, 32-bit PE, D3D9/D3D12/OpenGL. GDI support is limited to
  virtual display DCs and basic caps.
- Direct3D 11 coverage is a vertical slice: textures, depth/stencil, blending, rasterizer and
  sampler state, mip generation, copies and instancing are implemented in the translator, but only two
  paths are pixel-verified: the coloured triangle (216 pixels) and `d3d11_textured` (2x2 point-sampled
  texture, D32_FLOAT depth test with the near quad drawn first, 16-bit indexed draws, 3 quads).
  The triangle's visible canvas screenshot also checks its clear colour and centre pixel. Blending,
  alpha-test, mips/filtering, instancing and copies are not pixel-verified. Compute shaders/UAVs, geometry/tessellation, multisampling
  (forced to 1 sample), staging read-back of GPU-written data, and `SV_` system values beyond the
  basics are not implemented.
- **Pixel verification paths:** D3D11 back-buffer checks run in test mode (`?test=1`), which skips
  swap-chain presentation so the test can map the back buffer directly. A separate Chromium run
  without test mode captures the visible 640x480 canvas and checks the triangle's clear colour and
  interpolated centre pixel. Other render paths and browser engines remain unverified.
- Browser input (keyboard/mouse -> window messages) and the idle/frame scheduling in the worker are
  exercised by the Chromium browser suite. Firefox/Safari, visible canvas pixels outside the checked
  triangle case, large-game memory limits, and asset streaming remain unverified; see
  [`BROWSER_SUPPORT.md`](BROWSER_SUPPORT.md).
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.

## Remaining target-level and project work
- The section 1–6 checklist in `CLAUDE.md` tracks unfinished JIT, runtime, graphics, CPU, and
  productisation work. Broad items such as additional SEH/C++ dispatch, more D3D11 coverage, AVX,
  and 32-bit PE support need a representative target and dedicated design/verification before
  implementation.
- Three Diligent Win64 samples and the selected Diligent GLFW Demo maze have been built and analyzed.
  A D3D11-only `/MT` build now reports 96 missing static imports across the EXE and three DLLs,
  down from 349. Guest regressions cover Win32 `MultiByteToWideChar` and `LCMapStringEx` count-only
  queries. The bounded AnyWASM run resumes past condition-variable waits, initializes target static
  TLS, completes the selected MSVC C++ `std::runtime_error` catch, and reaches D3D11 shader/resource
  setup. Target-grounded CPU and HLE slices now carry it through a guest compute `Dispatch`, `Draw`,
  and repeated `IDXGISwapChain2::Present` calls. Native and WASM fixtures cover secure
  `SystemFunction036`; PE `.pdata` parsing, `RtlLookupFunctionEntry`, and a tested version 1
  `RtlVirtualUnwind` subset are in place. General SEH remains open. A browser fixture now executes
  one R32_FLOAT storage-texture dispatch, checks the UAV value by readback, and verifies a following
  draw samples it into a pixel. The selected target's shader, command stream in the browser, host
  rendering, and gameplay remain unverified. No real-game acceptance pass is claimed. See
  [`docs/TARGETS.md`](TARGETS.md) for exact traces and next steps.
- The D3D11 textured fixture has Rust command-stream and WASM smoke coverage, and its tested WebGPU
  rendering states have pixel checks in `test/browser/run.mjs`. Other rendering states listed in
  `CLAUDE.md` are not pixel-verified.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
