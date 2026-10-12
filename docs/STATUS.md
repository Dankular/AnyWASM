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
  indirect calls, GetProcAddress, thread synchronization, static TLS isolation, version checks,
  relative VFS paths, and `CreateDirectoryA/W` including empty directories and error cases; a
  mingw-w64 program imports the real `msvcrt.dll` (argv, qsort with guest callbacks,
  printf family, malloc/realloc, math, setjmp/longjmp, file I/O on the VFS, sscanf, atexit, exit
  code, stderr). The mingw program's output matches the same source compiled natively.
- `printf` engine: unit tests for flags/width/precision/length modifiers/floats.
- `anywasm` CLI (`analyze`, `run`, `pack`) and `.awb` bundles. `run` and `pack` support `--cwd`,
  bundles retain that guest directory, and the WASM loader restores it; relative Win32 and CRT file
  paths resolve against process state. Old bundles default to `C:\`. The same executables run through
  the **WebAssembly build** under Node (`test/wasm_smoke.mjs`) with output identical to native, and in
  **headless Chromium** through the real page and worker (`test/browser/run.mjs`). CLI `run` passes
  the remaining `--max-steps` budget to synchronous callbacks and accounts for retired instructions
  at frame stops; an integration fixture verifies a callback longer than the former 2,000,000-step
  slice completes. The current Asyncify-transformed WASM also passes the standalone
  `node test/wasm_smoke.mjs` suite.
- **Direct3D 11 -> WebGPU, end to end.** A mingw-compiled Win64 program using the real `d3d11.h`
  COM API (window class + `CreateWindowEx`, `D3D11CreateDeviceAndSwapChain`, buffers, DXBC shaders,
  input layout, `Map`/`Unmap`, `Present`) runs in the interpreter; COM vtables are generated from the
  mingw headers; calls become a binary GPU command stream (`tests/d3d11_stream.rs` checks it);
  `web/gpu.js` replays it with WebGPU through DXBC -> SPIR-V -> WGSL (vendored vkd3d-shader + Naga).
  The back buffer is read back and compared with analytic barycentric colours: 216 sampled pixels,
  worst error 0/255, zero WebGPU validation errors.
- Shader toolchain (`test/shaders.mjs`): HLSL -> DXBC -> SPIR-V -> WGSL with a fixed
  descriptor-to-bind-group layout (b#->group0, t#->group1, s#->group2, u#->group3 binding #+1);
  group3 binding 0 carries Naga's per-draw base-vertex/base-instance uniform. The selected target's
  indexed instanced draw values are pixel-tested. Its captured SDF compute shader also has a
  regression for integer-register bit patterns that vkd3d emits as non-finite f32 constants; it now
  translates via an in-function integer load and bitcast.
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
- A fresh six-round local sweep on 2026-10-12 measured medians of 24.15 / 19.7 / 23.15 / 33.75 M
  instr/s for cached-only compute, call-heavy, memory-heavy, and SSE-float fixtures. Forcing prefix 1
  measured 12.65 / 13.4 / 11.15 / 31.75 M instr/s; the SSE fixture emitted no instructions. This
  confirms the MOV emitter should remain disabled while JIT work addresses the memory and SSE paths.
  The raw-WASM benchmark and emitted-module counts are recorded in
  [`target-data/jit-benchmark-2026-10-12.json`](target-data/jit-benchmark-2026-10-12.json).
- Representative-target guest-cost sample (`test/target_frame_bench.mjs`): Diligent GLFW Demo,
  release WASM under Node 24.16.0/Windows x64, eight guest frame stops. Corrected process accounting
  includes synchronous callbacks, yielding HLE calls, and instructions retired before a block fault.
  In the final cached-only run, frame 1 used 580,777,731 instructions / 42.17 s; frames 2–8 each
  used 3,200 instructions. With forced MOV emission, frame 1 used 580,776,966 / 67.69 s; frames
  2–8 each used 3,200. Prefix 1 compiled 687,197 modules and executed 13,087,664 emitted blocks
  with no fallbacks. The cold prefix-1 pass was 60.5% slower in this sample. A same-build cached-only
  rerun differed by 638 first-frame instructions and 1.74 s, so more runs are needed to characterize
  variation. A further run after the atexit-budget fix used 580,771,924 instructions / 51.37 s
  cached-only and 580,774,467 / 76.18 s with prefix 1; both again reached eight frames. These
  repeats show substantial elapsed-time variation, not a stable speed comparison.
  These guest/HLE Node timings do not execute the target command stream through WebGPU and are not
  host frame times or a product performance target. The runtime enforces the run-slice cap; a
  synchronous callback that exhausts that slice fails loudly because it cannot be resumed, and an
  atexit callback budget failure is propagated instead of becoming a clean exit. Full data and
  reproduction commands are in `docs/TARGETS.md`.
- CI (GitHub Actions, user-provided workflow): `cargo build` + `cargo test` pass, including the
  hardware-differential suite, on the hosted runner.

## Known limits (not hidden)
- x87 is modelled with f64, not 80-bit; transcendentals compared to 1e-9 relative tolerance.
- `RCPPS`/`RSQRTPS` (and scalar `RCPSS`/`RSQRTSS`) use the interpreter's exact scalar
  `1.0 / x` and `1.0 / sqrt(x)` operations rather than a processor estimate. Intel's SDM and AMD's
  Architecture Programmer's Manual specify approximate results with maximum relative error
  `1.5 × 2^-12`, but do not specify exact output bits. Inference: compliant processor estimates can
  differ while staying within that bound. No per-model bit pattern is claimed, and the instructions
  remain excluded from bit-exact host differential checks. Sources: [Intel SDM, Volume 2B
  (RCPPS/RSQRTPS)](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf),
  [AMD64 Architecture Programmer's Manual, Volume 1](https://docs.amd.com/v/u/en-US/24592_3.24).
- Selected VEX instructions are decoded for target-used scalar/vector moves and arithmetic, XOR,
  rounding/conversion, `VEXTRACTF128`, `VZEROUPPER`, `ANDN`, `SHLX`, `SHRX`, and `BZHI` (see the
  exact list in `CLAUDE.md`). The target BZHI memory form is also checked against the host CPU on
  Linux CI across index boundary cases, excluding undefined AF/PF. This is not broad AVX/AVX2/BMI
  coverage; EVEX, FMA, MMX, AES/PCLMUL, and 16-bit/segment modes remain open.
- Diff corpus covers single instructions and x87 sequences; control flow, push/pop and syscall are
  covered only by the end-to-end programs.
- **Dynamic WASM emission (opt-in prototype, default disabled):** at a block entry, Rust emits a
  leading sequence of NOPs, 32-bit/64-bit GPR-immediate and GPR-to-GPR MOVs, selected 32-bit/
  64-bit memory MOVs, and immediate `JMP`/`Jcc` exits. Memory forms use plain, unsegmented 64-bit
  addressing and inline probes of
  the one-entry read/write TLBs; accesses must stay within one mapped page with compatible
  permissions. Cold misses, faults, page crossings, and writes to executable pages retry in the
  interpreter. A direct store ends its emitted prefix. Register-to-register MOV32 and memory
  loads zero-extend; self-moves retire normally. Conditional exits read copied RFLAGS unchanged;
  after a direct exit Rust resumes dispatch at the emitted RIP. The generated module imports the main runtime's
  memory and runs through `anywasm.run_emitted_block` in `web/anywasm.js`. It receives a 176-byte
  call state consisting of the 160-byte CPU snapshot (GPRs, RIP, RFLAGS, retired count, TSC) plus
  a 16-byte backing-memory/TLB view. Rust commits the CPU snapshot only after a valid exact
  instruction count. Compile/trap/invalid-count failures retry through the interpreter. The JS
  wrapper caches up to 256 generated modules and validates bytes if an ID is reused. Rust tests,
  `test/wasm_smoke.mjs` (including forced execution, one-instruction budgets, and same-bundle
  comparisons against cached-only and `{ jit: false }` for compute status, exit code, output,
  errors, and retired count) pass. The smoke confirms the default gate skips short prefixes and
  exercises safe interpreter retry after a host compile failure. Headless Chrome runs `compute.exe`
  through the emitter in the page and worker, compares output/counts, and grows WASM memory by one
  page after emission starts; later emitted calls continue and the cold-TLB fallback count does not
  increase. `jit_state.exe` enters a nine-instruction prefix with a warmed memory load and final
  store, then checks stored values; a second cold-page load checks interpreter retry. Its emitted
  path also takes a direct unconditional jump and a ZF-dependent conditional exit. WASM smoke
  compares emitted, cached-only, and interpreter status, exit code,
  output, errors, and retired count; Chromium compares page-realm output/counts with cached-only,
  and Windows runs the fixture natively. The fixture checks selected registers and memory, not
  arbitrary final process state; full JIT/interpreter/host differential remains open. This is a
  mechanism prototype, not a throughput win: emission requires explicit opt-in, and benchmarks
  show prefix-1 emission is slower on current compute, call-heavy, and memory-heavy fixtures.
  Partial-width MOVs, LEA, arithmetic, indirect/control-system exits, block chaining, and SSE/x87
  emission remain open. Module-cache pressure and emitted-path CSP checks have passed in Node, Chromium,
  Firefox, and WebKit; generated-module page/worker checks pass in the same browser set.
- Thread support uses a deterministic cooperative scheduler with saved CPU/stack/TEB TLS pointers,
  dynamic TLS/FLS maps, static PE TLS blocks, quantum rotation, resumable waits, events, mutexes,
  semaphores, critical sections, SRW locks, and condition-variable wake/reacquire behavior. An earlier
  bounded target run stopped at `SleepConditionVariableSRW` because its blocked guest continuation
  could not be resumed. A fresh 30,000,000-step CLI replay continued after that call and hit the step
  cap in D3DCompiler; a separate high-cap guest trace reached later guest calls. None of these runs
  verified a host window or renderer. Nested synchronous callback scheduling and alertable waits
  remain unsupported. A synchronous callback cannot suspend when its active run slice expires, so the
  runtime stops with a diagnostic instead of exceeding the budget. PE64 unwind metadata and a
  selected version 1 unwind subset are tested,
  including one target-grounded MSVC C++ catch; general SEH dispatch, vectored handlers, audio,
  DirectInput/XInput, 32-bit PE, D3D9/D3D12/OpenGL remain unsupported. GDI support is limited to
  virtual display DCs and basic caps.
- DLL initialization remains partial: bundled DLL `DllMain` calls follow module creation order,
  static TLS data is initialized, but TLS callbacks are not invoked. Named forwarded exports are
  resolved opportunistically, with failed names omitted. The selected Diligent EXE and three DLLs
  have no TLS callbacks or forwarded exports, so they do not verify those paths. The bounded target
  replay completed process attach for the D3D11 engine and D3DCompiler; Archiver was not observed
  loading. See [`docs/TARGETS.md`](TARGETS.md).
- Direct3D 11 coverage is a vertical slice: textures, depth/stencil, blending, rasterizer and
  sampler state, mip generation, copies and instancing are implemented in the translator. Focused
  pixel checks cover the coloured triangle (216 pixels) and `d3d11_textured` (2x2 point-sampled
  texture, D32_FLOAT depth ordering, indexed draws, constant-factor and source-alpha blending, and
  one texture plus one vertex-buffer `CopyResource` case). Target-level checks also cover visible
  SDF output, W movement, and Tab map regeneration. The triangle's visible canvas screenshot
  also checks its clear colour and centre pixel. Other blend modes, alpha-test, mips/filtering,
  instancing and other copies are not pixel-verified. Compute support is limited to a single
  R32_FLOAT storage-texture UAV; a small fixture checks its output, and the selected Diligent SDF
  shader now translates and dispatches but has no verified UAV output. D3D11 event queries report
  completion after `End` for the selected target's idle path; other query types remain unsupported.
  Structured/raw UAVs,
  geometry/tessellation, multisampling (forced to 1 sample), general staging read-back, and `SV_`
  system values beyond the basics remain unsupported.
- **Pixel verification paths:** D3D11 back-buffer checks run in test mode (`?test=1`), which skips
  swap-chain presentation so the test can map the back buffer directly. A separate Chromium run
  without test mode captures the visible 640x480 canvas and checks the triangle's clear colour and
  interpolated centre pixel. Other render paths and browser engines remain unverified.
- Browser input (keyboard/mouse -> window messages) and the idle/frame scheduling in the worker are
  exercised by the Chromium browser suite. The selected GLFW target also verifies W movement and
  Tab map regeneration after input routing skips its hidden helper window. Firefox/Safari, visible canvas pixels outside the checked
  triangle case, large-game memory limits, and asset streaming remain unverified. The web build is
  `wasm32-unknown-unknown`; the browser buffers the full `.awb`, copies it into WASM memory, and the
  bundle parser duplicates its files into the VFS. This source audit does not measure peak memory;
  see [`BROWSER_SUPPORT.md`](BROWSER_SUPPORT.md).
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.
- Anti-debugging, anti-cheat, and DRM behavior is not a supported compatibility target. No selected
  target currently requires it; revisit only if a representative target provides that evidence.

## Remaining target-level and project work
- The section 1–6 checklist in `CLAUDE.md` tracks unfinished JIT, runtime, graphics, CPU, and
  productisation work. Broad items such as additional SEH/C++ dispatch, more D3D11 coverage, AVX,
  and 32-bit PE support need a representative target and dedicated design/verification before
  implementation.
- Three Diligent Win64 samples and the selected Diligent GLFW Demo maze have been built and analyzed.
  `Tutorial01_HelloTriangle` and `Tutorial03_Texturing` have now been run with `--mode d3d11` under
  AnyWASM. Both reach virtual D3D11 device discovery/creation before stopping at the first unresolved
  `MSVCP140.dll` `basic_ios<char>` constructor. Static-CRT builds of both tutorials move past that
  import, complete D3D11 device and render/depth texture setup, then stop at unsupported VEX2
  `vsubss xmm1,xmm0,xmm2` before shader creation or `Present`. Neither tutorial has a pixel
  verification yet; Asteroids remains analyzer-only pending licence review.
  The current source-built, package-aware analyzer reports 84 missing static imports for the
  D3D11-only `/MT` build: 40 in the EXE, 9 in GraphicsEngineD3D11, 8 in Archiver, and 27 in
  D3Dcompiler. A prior status note listed 96, which this build does not reproduce; analyzing each
  image without `--dir` reports 88 because four imports from bundled D3Dcompiler are counted as
  missing. The older dynamic-runtime snapshot reported 349 with an older analyzer. Guest regressions
  cover Win32 `MultiByteToWideChar` and `LCMapStringEx` count-only
  queries. Scheduler regressions cover condition-variable resumption. An earlier bounded run stopped
  at `SleepConditionVariableSRW`; a fresh 30,000,000-step CLI replay continued after the wait and hit
  the cap in D3DCompiler. A separate high-cap guest trace initializes target static TLS, completes the
  selected MSVC C++ `std::runtime_error` catch, reaches D3D11 shader/resource setup, and advances
  through guest compute `Dispatch`, `Draw`, and repeated `IDXGISwapChain2::Present` calls. Those guest
  calls alone did not verify a host window or renderer. A later Chromium replay captures the
  1040x807 maze with 5 draws, one dispatch, and no WebGPU validation errors. After each `Stop::Frame`,
  the worker converts its measured 100 ns `requestAnimationFrame` wait into internal 10 ns ticks,
  matching the configured 10 MHz `QueryPerformanceCounter`; a process regression covers the scaling.
  In the post-fix seed-1 input replay, W/S/A/D and cursor-aimed movement each moved the player about
  22–24 pixels per 500 ms action, and the flashlight increased bright pixels by 2,035. GLFW's hidden
  helper HWND had received browser input; routing now selects the focused visible window or a visible
  top-level fallback. The random-map Tab replay changed 20.3% of captured pixels and issued a second
  compute dispatch. The seed-1 input replay reached 363 presents and the random-map reset replay
  reached 74, both without WebGPU validation errors. A target 128x128 R32_FLOAT UAV was independently
  read back. The frame is not compared with native output, and no general gameplay acceptance pass is
  claimed.
  Native and WASM fixtures cover secure
  `SystemFunction036`; PE `.pdata` parsing, `RtlLookupFunctionEntry`, and a tested version 1
  `RtlVirtualUnwind` subset are in place. General SEH remains open. A browser fixture now executes
  one R32_FLOAT storage-texture dispatch, checks the UAV value by readback, and verifies a following
  draw samples it into a pixel. A follow-up headless Chromium replay fixes the group's hidden
  draw-parameter binding and captures a nonempty 1040x807 frame from the selected target with one
  dispatch, five draws, and no WebGPU validation errors. The initial, W-movement, and Tab-regeneration
  frames are saved in `docs/images/` and can be reproduced with `test/browser/target.mjs`. On
  2026-10-12, a deterministic seed-1 route completed with the production 250,000-instruction worker
  slice; the callback yielded and resumed 16 times, and the teleport returned the marker within 1.27
  screen pixels of center. A follow-up with the rebuilt served runtime (SHA-256
  `A9E315E598191A80190EDB6B9AE0DBF1BA7C3855AC8F9B219E30DCC18B3DCC2A`) completed all 35 steps and
  1,834 input pulses over 1,858 presents, with 16 callback yields/resumes, two compute dispatches,
  and no WebGPU validation errors. The final marker was 0.29 pixels from its starting position. The
  replay record is
  [`target-data/glfwdemo-seed1-asyncify-replay.json`](target-data/glfwdemo-seed1-asyncify-replay.json).
  The fixed-direction flashlight check remains unreliable: the latest production-slice run gained 90
  bright pixels against its 100-pixel threshold (mean luminance delta 0.00947). Other movement states,
  broad gameplay acceptance, other UAV
  formats, and native comparison remain open. See [`docs/TARGETS.md`](TARGETS.md) for exact traces
  and limits.
- The D3D11 textured fixture has Rust command-stream and WASM smoke coverage, and its tested WebGPU
  rendering states have pixel checks in `test/browser/run.mjs`. Other rendering states listed in
  `CLAUDE.md` are not pixel-verified.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
