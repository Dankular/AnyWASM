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
  slice completes.
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
- Dynamic-emitter benchmark (node test/bench.mjs, Node 24.16.0/Windows x64, six rounds per
  workload, measured 2026-10-12) compared forced emission (jitMinPrefix 1) with cached-only
  execution (default threshold 65). Median guest instruction throughput in M instr/s:

  | Workload | Forced emission | Cached-only |
  | --- | ---: | ---: |
  | Compute | 8.1 | 22.1 |
  | Call-heavy | 9.2 | 19.0 |
  | Memory-heavy | 7.1 | 22.5 |
  | SSE-float | 50.8 | 34.7 |
  | Block-chain | 6.7 | 25.2 |

  Emission and block chaining were active in the forced run; the SSE loop compiled 10 modules and
  made 61,932 chain transitions per round. This sample shows a gain on the SSE-float fixture, but
  lower throughput on the other four, so emission stays disabled by default. The SSE workload is
  short and its elapsed times vary; this is a local fixture result, not a general speedup claim.
  Prefix 65 remains above the 64-instruction block limit and skips emission. These cached-only
  rates are the same-build comparison for this run; older benchmark samples above are historical.
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
- **Dynamic WASM emission (opt-in prototype, default disabled):** Rust emits leading NOPs,
  32-bit/64-bit GPR-immediate and GPR-to-GPR MOVs, selected TLB-backed 32-bit/64-bit memory MOVs, 32-bit ADD
  with register or immediate source and arithmetic flags, direct JMP/Jcc exits, full-width vector
  moves, and packed-double ADD/MUL. Memory forms use plain unsegmented 64-bit addresses and
  one-entry read/write TLB checks; accesses must remain in one mapped page with compatible
  permissions. Cold misses, faults, page crossings, and writes to executable pages retry through
  the interpreter. A direct store ends its emitted prefix. Packed-double arithmetic falls back for
  non-default MXCSR rounding/DAZ/FTZ controls and zero, subnormal, infinite, or NaN operands.
  Other instructions, including x87 and unsupported SSE forms, remain interpreted.
  The generated module imports the runtime memory and runs through anywasm.run_emitted_block in
  web/anywasm.js. Its 624-byte call state holds the integer CPU snapshot, memory/TLB pointers,
  counters and scratch, XMM values, and MXCSR. GPR and XMM values are block-local Wasm locals
  flushed on exit. Rust commits copied CPU state only after an exact valid instruction count;
  compile, trap, invalid-count, and guarded-operation failures retry against the untouched snapshot.
  The JS wrapper caches up to 256 modules and uses a balanced RIP lookup tree to chain among
  registered emitted block starts.
  Rust JIT tests and test/wasm_smoke.mjs pass. Smoke checks compare emitted execution with
  cached-only and interpreter output/counts for compute and SSE fixtures, validate both packed
  result lanes, verify 32-bit ADD/Jcc loop chaining, and check selected registers and memory in
  jit_state.exe. Headless Chromium page and worker tests pass; the page test grows WASM memory by
  one page after emission starts and confirms the cached module resumes without another TLB fallback.
  The current full Chromium browser-suite run has one separate D3D11 structured-buffer UAV shader
  validation failure. The fixture covers selected registers and memory, not arbitrary final process
  state; complete JIT/interpreter/host differential testing remains open.
  A fresh same-build six-round sample shows prefix-1 emission below cached-only throughput on four
  of five fixtures, while the SSE-float fixture is faster. The result is workload-specific, and
  emission remains disabled by default. Partial-width MOVs, LEA, wider integer arithmetic, indirect
  and control-system exits, and broader SSE/x87 emission remain open. Module-cache pressure and
  emitted-path CSP checks pass in Node, Chromium, Firefox, and WebKit.
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
  `MSVCP140.dll` `basic_ios<char>` constructor. They do not reach shader creation, guest `Present`,
  or pixel verification; Asteroids remains analyzer-only pending licence review.
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
  frames are saved in `docs/images/` and can be reproduced with `test/browser/target.mjs`. The normal
  worker slice still stops in a synchronous callback that cannot resume. Target UAV values and native
  comparison remain unverified; other movement directions and gameplay acceptance are open. See
  [`docs/TARGETS.md`](TARGETS.md) for exact traces and limits.
- The D3D11 textured fixture has Rust command-stream and WASM smoke coverage, and its tested WebGPU
  rendering states have pixel checks in `test/browser/run.mjs`. Other rendering states listed in
  `CLAUDE.md` are not pixel-verified.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
