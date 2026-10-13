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
- `anywasm-rt`: PE32/PE32+ loader (relocations, imports, delay imports, validated x64 `.pdata`, per-thread static TLS, TEB/PEB with loader lists,
  bundled DLLs) + host-implemented kernel32/ntdll/CRT. 31 end-to-end tests run **real Windows x64
  executables**: freestanding fixtures cover console I/O, relocated pointer tables, heap, SSE2 math,
  indirect calls, GetProcAddress, thread synchronization, static TLS isolation, version checks,
  relative VFS paths, and `CreateDirectoryA/W` including empty directories and error cases; a
  mingw-w64 program imports the real `msvcrt.dll` (argv, qsort with guest callbacks,
  printf family, malloc/realloc, math, setjmp/longjmp, file I/O on the VFS, sscanf, atexit, exit
  code, stderr). The mingw program's output matches the same source compiled natively.
  Synthetic PE32 coverage also checks TLS callback stdcall arguments and private static TLS blocks
  through the four-byte TEB vector at `TEB+0x2c`.
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
  translates via an in-function integer load and bitcast. DXBC fixtures generated from the target's
  `DrawMap.hlsl` exercise its vertex and ray-marching pixel shaders, including their descriptors and
  control flow; Chromium validates the resulting WebGPU render pipeline and checks that a wall SDF
  sample shades darker than a clear-space sample.
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
  host frame times or a product performance target. The native CLI enforces the run-slice cap; a
  synchronous callback that exhausts it cannot resume and fails loudly, and an atexit callback
  budget failure is propagated instead of becoming a clean exit. In WASM/browser, Asyncify can
  resume callbacks across worker slices; one normal-slice target replay is recorded in
  `docs/TARGETS.md`. Full data and
  reproduction commands are in `docs/TARGETS.md`.
- CI (GitHub Actions, `.github/workflows/rust.yml` and `web.yml`; `ci/web.yml` mirrors the latter):
  the `Rust` and `WASM` workflows passed through commit `bd6f0bd`, including the
  hardware-differential suite on the hosted runner. **As of 2026-10-13 they are failing** on the
  branch head `ef301dd` and on the 20+ preceding pushes I inspected. The job log for the `Rust`
  run on `4caaf0c` shows `error[E0432]: unresolved import crate::mem` at
  `crates/anywasm-cpu-state/src/../../anywasm-cpu/src/x87.rs:521` when building the
  `anywasm-cpu-state` lib tests; `cargo test -p anywasm-cpu-state --no-run` succeeds locally, so
  the cause is not yet established (not investigated further; the `WASM` workflow failure reasons
  were not read). Treat "tests pass" claims in this file as local results until CI is green.

## Dynamic WASM emission: state as of 2026-10-13
- Until commit `a51b636` **every emitted module was invalid** (the local-group count in `emit_module`
  was one short, and `emit_inc32` stored RFLAGS without an address). V8 rejected each module, the
  runtime silently fell back to the interpreter, and the JIT tests that assert `compilations > 0`
  were failing. With forced emission, `compute.exe` now compiles 2,127 modules and runs 363,707
  emitted blocks with 0 fallbacks.
- `node test/wasm_smoke.mjs` (jit-test-api build) still reports **10 failures**, all JIT-specific
  (cold-page TLB fallback count, packed-double, x87, indirect-JMP chaining, memory-add chaining).
  The same 10 fail on a build from before the shared-memory work plus only the two fixes above, so
  they are not caused by that work. Several show `compilations: 0, fallbacks: 0` (emission is not
  attempted for those fixtures at all); causes not yet investigated. Output correctness of the
  emitted path is therefore **not yet established** by this suite.

## WebGPU executor profile (2026-10-13, synthetic proxy)
`node test/browser/profile-gpu.mjs` (headless Chromium, software-or-virtual adapter of this sandbox)
runs `fixtures/d3d11_texload` (136 textures incl. 8 mip-mapped 1024x1024, 4 uploads per frame,
20 frames) with `?profileGpu`. This is a stand-in for a loading phase, not a real title, and it
measures only JavaScript time spent issuing commands, not GPU execution time.
- 524 ms of JS time over the run; `TEXTURE_WRITE` is 86% of it (216 writes, 456 MiB, 449 ms,
  worst single write 40 ms), then `CREATE_SHADER` 11% (2 shaders, 56 ms, one-time).
- Everything else (draws, presents, pipeline/state, buffer writes) totals under 3%.
- The same run retired 17.0 M guest instructions at 4.2 M instr/s (about 4 s), so guest CPU
  time, not command issue, dominated wall time here (4.4 s). Moving this data-copy work onto GPU
  compute would not help: `TEXTURE_WRITE` is a memory-bandwidth copy (~1 GiB/s), not arithmetic.
  Removing a copy on that path is the candidate; it has not been attempted.

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
- Selected VEX instructions are decoded for target-used scalar/vector moves and arithmetic,
  including packed-single `VADDPS`/`VMULPS`, `VPERMILPS`/`VPERMILPD`, XOR, rounding/conversion,
  `VEXTRACTF128`, `VZEROUPPER`, `ANDN`, `SHLX`, `SHRX`, and `BZHI` (see the exact target-grounded
  list in `CLAUDE.md`). BMI coverage includes BMI1 `ANDN`, `BEXTR`, `BLSI`, `BLSMSK`, `BLSR`, and
  `TZCNT`, plus BMI2 `BZHI`, `MULX`, `PDEP`, `PEXT`, `RORX`, `SARX`, `SHLX`, and `SHRX`; BZHI
  handles both 32-bit and 64-bit operands. The target BZHI memory form is also checked against the
  host CPU on Linux CI across index boundary cases, excluding undefined AF/PF. Guest CPUID
  advertises BMI1 and BMI2 now that their instruction families are implemented; incomplete feature
  families remain hidden. Encoding and behavior references: [Intel SDM, Vol. 2A](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2a-manual.pdf),
  [Vol. 2B](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf),
  and [Vol. 1 CPUID definitions](https://cdrdv2-public.intel.com/868137/325462-089-sdm-vol-1-2abcd-3abcd-4.pdf).
  AVX/AVX2 coverage remains incomplete; EVEX, FMA, MMX, AES/PCLMUL, and 16-bit/segment modes remain
  open.
- Diff corpus covers single instructions and x87 sequences; control flow, push/pop and syscall are
  covered only by the end-to-end programs.
- **Dynamic WASM emission (opt-in prototype, default disabled):** Rust emits leading NOPs,
  selected 32-bit/64-bit GPR MOVs and TLB-backed memory MOVs, 32-bit ADD, memory ADD, direct JMP/Jcc
  and register-indirect JMP exits, full-width vector moves, guarded packed-double ADD/MUL/SUB/DIV,
  and a narrow f64-model x87 register-stack subset. Memory probes use plain unsegmented 64-bit
  addresses and the existing one-entry read/write TLBs; accesses must stay within one mapped page
  with compatible permissions and current frame bounds. Misses, faults, page crossings, and
  executable-page stores retry through the interpreter. Mapping/protection changes and executable
  writes invalidate cached translations. Direct stores end an emitted prefix.
  Packed-double arithmetic is guarded for the supported finite normal/default-MXCSR domain;
  signed zero, subnormal, infinity, NaN, and unsupported MXCSR modes use interpreter fallback.
  Tests compare full state for those cases and for packed-double memory-source arithmetic, which
  stays interpreted. x87 emits FLD/FXCH/FSTP register-stack forms and all six selected D8 ST
  arithmetic selectors under the interpreter's f64 model. The smoke fixture checks all six and an
  exceptional arithmetic fallback against complete CPU/memory snapshots; memory forms and extended
  precision remain unsupported.
  The generated module imports runtime memory and runs through `anywasm.run_emitted_block` in
  `web/anywasm.js`. Its 976-byte call state holds every current CPU field plus memory/TLB pointers,
  counters, and scratch. GPR/XMM values are block-local Wasm locals flushed on exit. Rust commits the
  full copied CPU state only after an exact valid instruction count. The feature-gated smoke API
  compares all CPU fields and all mapped guest pages across emitted, cached-only, and interpreter
  execution for compute, packed-double, x87, indirect-JMP, memory, and state fixtures. Compile,
  trap, invalid-count, and guarded-operation failures retry from the untouched snapshot. The JS
  wrapper caches up to 256 modules and chains only among registered emitted block starts through a
  balanced RIP lookup tree. CALL remains interpreted; unregistered indirect targets return to
  normal lookup/fault handling and HLE targets return to the runtime dispatcher.

  The memory tests cover four interleaved hot pages, protection/map/unmap invalidation, cross-page
  faults, and executable-page write invalidation. The emitted memory fixture walks multiple pages
  and compares complete CPU/memory state. The Chromium page and worker checks compare output/counts;
  the page test grows WASM memory by one page after emission starts and confirms execution resumes
  with matching cached-only output/count. The JIT-specific browser checks pass. A fresh full browser
  suite on Chrome 154.0.8037.98 passed the JIT, input, graphics, and compute assertions, including
  mouse-coordinate scaling, mouse-leave delivery, and visible-canvas pixel checks. It reported no
  page errors or WebGPU validation errors. Module-cache pressure
  and emitted-path CSP checks pass in Node, Chromium, Firefox, and WebKit.

  Latest paired Node benchmark: one warm-up plus 10 alternating cached-only/emitted rounds on one
  release WASM build (`jitMinPrefix` 65/1, five-million-instruction slices). Every pair matched
  status, exit code, stdout/stderr, and retired count. Values are median guest MIPS; emitted counts
  are from the representative final pair.

  | Workload | Cached | Emitted | Change | Modules | Executions | Chain transitions | Register hits | Fallbacks |
  |---|---:|---:|---:|---:|---:|---:|---:|---:|
  | Compute | 20.14 | 6.28 | -68.8% | 67 | 365,969 | 2,287 | 283,595 | 2 |
  | Call-heavy | 17.32 | 4.31 | -75.1% | 5 | 1,500,199 | 499,998 | 0 | 1 |
  | Memory-heavy | 19.61 | 65.14 | +232.2% | 11 | 782,888 | 782,524 | 261,014 | 2,233 |
  | SSE-float | 26.35 | 42.74 | +62.2% | 10 | 62,599 | 61,932 | 0 | 1 |
  | Block-chain | 21.12 | 5.57 | -73.6% | 28 | 200,102 | 100,079 | 200,001 | 2 |

  The memory and SSE medians exceed the 10% acceptance target. The other three workloads regress,
  so emission remains disabled by default. The existing one-entry TLB stays in place; a multi-entry
  alternative was not implemented or directly benchmarked. Partial-width MOVs, LEA, wider integer
  arithmetic, packed-single SSE arithmetic, CALL, system/runtime exits, broader x87 exception
  behavior, and arbitrary-program full-state differential coverage remain open. The latest
  `cargo test --workspace` run passes.
- Thread support uses a deterministic cooperative scheduler with saved CPU/stack/TEB TLS pointers,
  dynamic TLS/FLS maps, static PE TLS blocks, quantum rotation, resumable waits, events, mutexes,
  semaphores, critical sections, SRW locks, and condition-variable wake/reacquire behavior. An earlier
  bounded target run stopped at `SleepConditionVariableSRW` because its blocked guest continuation
  could not be resumed. A fresh 30,000,000-step CLI replay continued after that call and hit the step
  cap in D3DCompiler; a separate high-cap guest trace reached later guest calls. None of these runs
  verified a host window or renderer. Nested synchronous callback scheduling and alertable waits
  remain unsupported. The native CLI cannot suspend a synchronous callback when its active run
  slice expires and stops with a diagnostic; the WASM/browser path uses Asyncify and has passed the
  selected target's normal worker-slice callback-resumption gate. PE64 unwind metadata and a
  selected version 1 unwind subset are tested, including one target-grounded MSVC C++ catch.
  Ordinary x64 `RaiseException` records now search registered vectored handlers and
  `__C_specific_handler` scope tables, with synthetic coverage for handler order, filters, catch-all
  scopes, continuation, termination handlers, and target unwind. The source now also dispatches
  vectored-continue callbacks on resumed exceptions and invokes the registered top-level filter
  after an exhausted search; synthetic coverage checks callback ordering, filter replacement, and
  context resumption. Selected x64 access violations, integer divide errors, illegal instructions,
  privileged instructions, and software breakpoints now enter this dispatcher; synthetic tests
  check Windows exception records, CONTEXT.Rip edits, and MXCSR round-tripping. A first PE32 slice
  now writes x86 EXCEPTION_RECORD and CONTEXT layouts, invokes vectored handlers and linked
  NT_TIB.ExceptionList handlers, resumes edited contexts, and honors RaiseException's four-argument
  stdcall frame. Guest execution covers selected x86 illegal, privileged, divide, and breakpoint
  exceptions, while a scheduler regression checks per-thread ExceptionList state. PE32 now also
  handles the MSVC EH4 C SEH path: the image-local handler's cookie-encoded scope table, filter
  continue-search/continue-execution/execute-handler results, and normal plus unwind-time
  `__finally` behavior are covered by a built x86 fixture. PE32 nested and collided unwind plus
  broader compiler-specific C/C++ runtime behavior remain open. A direct-entry PE32 EH3 fixture now
  verifies by-value/reference catches, catch-all, nested catches, and destructor
  unwind. General Win32/PE32 compatibility remains incomplete. Other CPU exits, audio, DirectInput/
  XInput, general D3D9, D3D12, and OpenGL remain unsupported. The D3D9
  fixed-function subset covers the checked FVF layouts, stage-0 SELECTARG1/SELECTARG2,
  MODULATE/MODULATE2X, ADD/SUBTRACT, ADDSMOOTH, signed-add and modulate-add operations,
  BLENDTEXTUREALPHA/BLENDFACTORALPHA, DOTPRODUCT3, triangle fans, alpha blending, and alpha
  testing across all comparison functions at the equality boundary. GDI support is
  limited to virtual display DCs and basic caps.
- Browser guest execution has an opt-in SharedArrayBuffer CPU-worker path. The 2026-10-12
  feasibility probe reached two concurrent no-std Wasm workers with disjoint contexts/stacks, an
  exact shared atomic count, and a measured maximum concurrency of two. Full-runtime-shaped
  instances alias mutable statics and a stand-in cursor value; the probe does not exercise `std`'s
  allocator, and the production runtime still cannot link against shared memory because its
  prebuilt `std` lacks atomics/bulk-memory. A coordinator/CPU-worker split is documented in
  [`sharedarraybuffer-guest-threads-design.md`](superpowers/specs/2026-10-12-sharedarraybuffer-guest-threads-design.md);
  `anywasm-cpu-state` contains the `no_std` fixed-layout CPU context, decoder, generic instruction
  executor, versioned shared-memory header, atomic worker-slot handoff, and permission-checked
  shared-frame view. A separate `anywasm-cpu-worker` module claims a slot, validates its context and
  map generation, runs bounded integer/control-flow and SSE slices against copied shared frames,
  stops at the coordinator's HLE-thunk range, and publishes its full CPU context. The process
  snapshot ABI and browser copy-back are wired into the opt-in `?sharedCpuWorkers` path. A
  cross-origin-isolated headless Chrome smoke started the CPU worker and ran `hello.exe` to exit 0.
  The two-thread `parallel_test.exe` browser fixture also requires shared-worker mode, a batch with
  two guest contexts, more than 500,000 worker-retired instructions, and exact prime counts, dynamic
  TLS values, per-thread TEB last-error values, and a locked shared total. A second fixture verifies
  a worker-reported breakpoint returns to the coordinator's vectored exception handler and resumes;
  a simulated worker-module HTTP 503 verifies that startup failure leaves the guest usable in
  cooperative mode. A worker stop now restores the original RIP for instructions that advance RIP
  before returning an exception, so the coordinator can dispatch it once. These fixtures verify
  production-pool handoff and guest results. The browser launcher now enables this path only when
  `sharedCpuWorkers` is explicitly present; an absent parameter stays cooperative as the design
  requires. `test/browser/cpu-mode-default.test.mjs` verifies the default, and the explicit shared
  `parallel_test.exe` run still passes with two workers, 888,720 worker-retired instructions, and five
  two-context batches. Before this correction, an absent parameter selected shared mode: the
  Tutorial03 first-frame capture recorded 4,174,472 Asyncify yields for 30 presents, while cooperative
  mode recorded 16 for the same frame count. The separate feasibility probe is the direct
  measurement of simultaneous worker execution. The coordinator's runnable-context query omits blocked,
  suspended, exited, and stale ready-queue entries; PE32 remains active-context-only while it uses
  the fixed TEB alias. Target-level acceptance remains open. The worker artifact uses the unstable
  `+atomics` feature and LLD `--no-check-features`
  because the installed sysroot core objects lack shared-memory feature metadata. A separate
  HLE-only Tutorial01 probe confirmed cross-origin isolation and rendered 719 draws and 360 presents
  in 30 seconds with no WebGPU errors; the process remained active, so target completion is not
  established. A paired first-frame GLFW Demo replay with the current source-built runtime reached
  one SDF dispatch in both cooperative and shared-worker modes. The shared pool created two workers
  and retired 2.8 million instructions, but recorded zero two-context parallel batches; this target
  did not exercise overlap. Both captures remained in `Starting`, with no import stop, terminal
  status, or WebGPU validation errors. See the paired counters and pixel metrics in
  [`TARGETS.md`](TARGETS.md#shared-cpu-worker-target-comparison-2026-10-13). Shared memory now uses
  worker/coordinator dirty-page tracking, cached mapping rows,
  and full refreshes after mapping or protection changes. Shared-memory atomic operations outside
  the worker backend's supported width or alignment stop at the faulting instruction with a
  distinct coordinator-fallback reason, so the serialized interpreter retries them. The live probe
  found and fixed a scheduler fallback that stepped one instruction per turn after a worker quantum
  expired. Worker completions now distinguish process/thread return trampolines from ordinary HLE
  stops; the coordinator still owns the corresponding exit handling. The target trace
  and limits are in `docs/TARGETS.md`.
- DLL initialization remains partial: bundled DLL TLS callbacks and `DllMain` calls now follow
  dependency-first loading, with callbacks invoked in image order before each DLL entry point. EXE
  callbacks run after DLL initialization and before the EXE entry point. Synthetic regressions cover
  callback order, process-attach arguments, EXE startup, and the DLL dependency chain. A PE32
  regression now exercises a three-image dependency chain, TLS-before-`DllMain` ordering, and x86
  callback arguments. Static TLS data is initialized. Named forwarded exports are resolved
  opportunistically; failed names are omitted. The selected Diligent EXE and three DLLs have no TLS
  callbacks or forwarded exports, so
  they do not verify those paths. The bounded target replay
  completed process attach for the D3D11 engine and D3DCompiler; Archiver was not observed loading.
  See [`docs/TARGETS.md`](TARGETS.md).
- Direct3D 11 coverage is a vertical slice. Focused browser pixel/readback checks cover the coloured
  triangle (216 pixels), constant-factor and source-alpha blending, reverse-subtract/add/subtract/
  min/max blending, texture/vertex/index-buffer copies, texture region copies and uploads, including
  a partial RG8_UNORM texel copy and a BC1 block copy that preserve neighboring destination data,
  DXBC clip/discard, 2D and 3D mip generation, R8_UNORM/RG8_UNORM/R16_UNORM/R16_SNORM Texture3D
  samples with outside and edge pixels, fractional-LOD linear filtering, an 8x anisotropic
  sampler request, instance step rates, render-to-texture, multiple render targets, cube/array/3D
  texture sampling, depth/stencil, rasterizer state, 4x MSAA resolve, D24S8 depth with
  alpha-to-coverage and complementary sample masks, and D3D11.1 constant-buffer ranges. The D3D9 fixed-function slice now has pixel checks for selected FVF layouts, triangle-fan
  rendering, stage-0 SELECTARG1/SELECTARG2, MODULATE/MODULATE2X, ADD/SUBTRACT, ADDSMOOTH,
  signed-add, modulate-add, texture/factor-alpha blending, DOTPRODUCT3, and all alpha-test
  comparison functions at equality. These
  checks do not establish broad API or game compatibility. Target-level D3D11 checks cover visible
  SDF output, W movement, and Tab map regeneration; two fresh random-map runs measure a flashlight
  response along an unobstructed SDF ray. The triangle canvas screenshot checks its clear colour
  and centre pixel.
  Four-sample depth tests also cover D32_FLOAT, D24S8, and D16_UNORM attachments; each keeps the
  nearer draw through an RGBA8 color resolve. `CheckFormatSupport` reports depth-stencil support
  only for typed DSV formats. Remaining D3D11 gaps include broader copy/mip/format
  combinations, more format/resolve and sampler pixel comparisons, render-state cross-products,
  non-event query semantics, geometry/tessellation, and additional staging layouts. Deferred contexts
  and command lists have focused Rust tests for creation/type/flags, recording, finish/reset, execute
  ordering, and state restoration. Compute browser fixtures cover
  typed R32/RGBA textures, structured/raw/typed buffer ranges, and a 3D texture UAV. The selected
  Diligent SDF shader dispatches and produces finite values on readback; broader format coverage and
  pixel equivalence remain unverified.
- **Pixel verification paths:** D3D11 back-buffer checks run in test mode (`?test=1`), which skips
  swap-chain presentation so the test can map the back buffer directly. A separate Chromium run
  without test mode captures the visible 640x480 canvas and checks the triangle's clear colour and
  interpolated centre pixel. Other render paths and browser engines remain unverified.
- Browser input (keyboard/mouse -> window messages) and the idle/frame scheduling in the worker are
  exercised by the Chromium browser suite. The selected GLFW target also verifies W movement and
  Tab map regeneration after input routing skips its hidden helper window. Firefox/Safari, visible
  canvas pixels outside the checked triangle case, and large-game memory limits remain unverified.
  GTA's AGP header/index and requested byte ranges have been exercised in Chromium; the package
  library verifies the AWB and index hashes during install and keeps the AGP payload remote. A
  comparable speed result for the latest bounded range scheduler remains open. The web build is
  `wasm32-unknown-unknown`; the browser buffers the full `.awb`, copies it into WASM memory, and the
  bundle parser duplicates its files into the VFS. This source audit does not measure peak memory;
  see [`BROWSER_SUPPORT.md`](BROWSER_SUPPORT.md).
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.
- Anti-debugging, anti-cheat, and DRM behavior is not a supported compatibility target. No selected
  target currently requires it; revisit only if a representative target provides that evidence.

## Remaining target-level and project work
- The section 1–6 checklist in `CLAUDE.md` tracks unfinished JIT, runtime, graphics, CPU, and
  productisation work. Additional SEH/C++ dispatch, more D3D11 coverage, AVX, and 32-bit PE support
  remain broad workstreams; the existing PE32 C++ exception design is being implemented in slices.
- A focused x64 MSVC C++ exception workstream now has EH3/EH4 metadata readers, catch matching,
  and `/MD` compiler fixtures. Local MSVC source confirms that EH3 `dispFrame` applies while
  execution is already inside a catch; try-body catches use the establisher frame. The HLE-only
  guest runs for both FH3 and FH4 fixtures now return 0 under a 2,000,000-step cap, including
  adjusted base-reference, catch-all, nested catch, and unwind-funclet paths. The focused EH suite
  passes 30 tests; the runtime library suite passes 172. HLE-only runs and runs with the installed
  VCRUNTIME redist plus the actual system `ucrtbase.dll` both return 0. Runtime traces confirm guest
  execution in `ucrtbase.dll` and VCRUNTIME; API-set contracts without physical VFS DLLs still use
  HLE. The host `--workspace` build succeeds: the worker crate enables `no_std` only on `wasm32` and
  uses the host `std` runtime elsewhere. The shared-memory `wasm32` worker build also succeeds with
  atomics and imported-memory linker flags. Browser execution of the exception fixtures remains.
  PE32 EH3 now reads absolute-pointer throw/function metadata, searches the x86 registration state's
  active try blocks, and contains a bounded second-pass unwind to a selected registration frame.
  The dispatcher sets the x86 unwind flags, runs intervening frame handlers, unlinks frames above
  the target, and resumes through the selected catch funclet. A compile-only MSVC 19.29 x86 assembly
  probe confirmed compiler-generated handler thunks place the absolute FuncInfo pointer in EAX
  before jumping to `__CxxFrameHandler3`. A direct-entry x86 MSVC fixture now runs under the HLE-only
  runtime and returns 0 for by-value and adjusted-reference catches, catch-all, nested try/catch, and
  destructor execution during unwind. This exercised the PE32 EH3 search, two-pass unwind, by-value
  copy, and catch transfer. The catch funclet runs with its parent EBP and saved throw-site ESP, then
  returns the post-catch continuation in EAX. A new cross-frame fixture case throws from a noinline
  callee with an active destructor and catches by base reference in its caller; native x86 and HLE-only
  direct-entry and normal-startup runs all returned 0 under the 2,000,000-step cap. This verifies
  caller/callee registration transfer and inner-frame cleanup for this case. A `ThrowLifetimeProbe`
  case now verifies the exception object survives through its by-reference catch and its ThrowInfo
  unwind function runs exactly once afterward; native x86 startup/direct and x64 EH3/EH4 fixtures,
  plus their HLE-only variants, all return 0. PE32 CRT initializer
  arrays now use four-byte entries, and `InitializeSListHead` clears the 8-byte x86 or 16-byte x64
  header. A normal `/MD` PE32 startup
  now reaches `crt!exit` and returns 0; the startup trace exposed and fixed its missing stdcall cleanup
  for `InitializeSListHead`. `_except_handler4_common` now decodes the PE32 registration and scope
  records, checks image-local security cookies, runs filters and `__finally` handlers, and transfers
  to the selected `__except` block. A normal-startup x86 fixture returns 0 after checking continue
  search, continue execution, exception-code access, and finally execution during unwind. This is a
  compiler-targeted EH4 subset; broader compiler-specific layouts, nested/collided SEH unwind
  dispositions, and wider PE32 API compatibility remain unsupported. The analyzer now accepts
  supported I386/PE32 and AMD64/PE32+ pairs.
- Three Diligent Win64 samples and the selected Diligent GLFW Demo maze have been built and analyzed.
  Historical `Tutorial01_HelloTriangle` and `Tutorial03_Texturing` runs with `--mode d3d11` reached
  virtual D3D11 device discovery/creation before stopping at the unresolved `MSVCP140.dll`
  `basic_ios<char>` constructor. The runtime now maps MSVCP imports to a dedicated HLE group and
  initializes the observed narrow, UTF-16, and wide `basic_ios` constructor layouts. A local
  `Tutorial01_HelloTriangle` probe package with the installed MSVC runtime DLLs and UCRT then passed
  DLL initialization, the observed one-package processor-topology query, and D3D11 resource setup;
  its 30,000,000-step trace hit the cap in D3DCompiler. A separate 100,000,000-step run hit the cap
  at `Tutorial01_HelloTriangle.exe+0x2cbb` in a string-buffer growth path. The later 250,000,000-step
  Release replay was stopped before its result was captured. With a separate local probe package and
  the installed MSVC/UCRT DLLs, `Tutorial03_Texturing` reached two guest vertex/pixel shader-object
  creation sequences, input layouts, and buffer creation before its 100,000,000-step cap at
  `tutorial03_texturing.exe+0x44280`. Its trace contains no guest `Present` or unresolved-import stop.
  A current-source HLE-only CLI probe without MSVC/UCRT DLLs reached D3DCompiler `HeapAlloc` calls,
  then stopped at the synchronous callback budget; no unresolved MSVCP call appeared in that short
  trace. The current-source analyzer report lists 210 implemented / 41 missing static imports for
  Tutorial03, including 21 of 65 MSVCP140 imports and 3 of 18 VCRUNTIME140 imports still missing.
  The full report is in `docs/target-imports/tutorial03-current-source.json`; its preflight is static
  review only. These local DLLs are not a product package. The first
  browser report with that package stopped at unsupported `VPERMILPS xmm0,xmm5,0x55` with zero GPU
  work. The CPU source now handles target-used `VPERMILPS`/`VPERMILPD` and packed-single
  `VMULPS`/`VADDPS`; the release browser Wasm was rebuilt and Asyncify-processed from current source.
  A bounded current-source capture passed that instruction sequence and showed the textured Diligent
  cube on the page canvas, with 59 draws, 30 presents, two pipelines, and zero WebGPU validation
  errors. The report captures on first GPU stats, before terminal process status, and logged one 404
  resource error; later execution remains unverified. A later current-source HLE-only browser replay
  without local MSVC/UCRT DLLs passed the stream, clock, and numeric formatting imports and continued
  rendering: the latest 45-second report recorded 2,759 draws, 1,380 presents, 5,519 passes, two
  pipelines, and zero WebGPU validation errors. It remained in `Starting` when the report timed out,
  with no terminal process status. The only 404 was the browser's `/favicon.ico` request; the page
  context reported no failed requests or HTTP errors for sample resources. An earlier HLE-only CLI capture stopped at
  `tutorial03_texturing.exe+0x34fc` after 11,248 guest instructions while reading the vtable for an
  uninitialized auxiliary ostream during insertion of the ANSI string `\x1b[39m`; that capture
  predates the current stream handlers. An earlier analyzer snapshot reported 165 implemented /
  86 missing imports, including 61 of 65 MSVCP140 imports missing. The current-source report now
  records 210 implemented / 41 missing, including 21 of 65 MSVCP140 imports missing. The local
  runtime DLLs are not a product dependency or redistribution decision.
  A separate HLE-only browser bundle of Tutorial01_HelloTriangle, also without local MSVC/UCRT DLLs,
  now renders the colored triangle at the normal 250,000-instruction worker slice: 659 draws, 330
  presents, two pipelines, 16 Asyncify yields/resumes, and no WebGPU validation errors. Its page
  remained in `Starting` and the report recorded one generic console 404 without identifying a failed
  HTTP response. The capture details are in `docs/TARGETS.md`.
  A 2026-10-13 cooperative-mode recheck captured the Diligent triangle and textured cube directly from
  1280x1024 backbuffers, with 59 draws, 30 presents, 16 Asyncify yield/resume pairs, and no WebGPU errors
  per sample. No unresolved runtime MSVCP import appeared before either frame; the current source
  analyzer's static MSVCP misses remain outside these exercised paths. See
  `docs/target-data/diligent-hle-only-browser-2026-10-13.json`.
  Asteroids remains analyzer-only pending licence review.
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
  `SystemFunction036`; PE `.pdata` parsing and `RtlLookupFunctionEntry` over static and registered
  dynamic function tables work through ntdll and forwarded kernel32 imports. `RtlAddFunctionTable`
  and `RtlDeleteFunctionTable` now register and remove guest-owned tables. Callback and growable
  function-table APIs remain open. The dynamic-table path is build-checked but not runtime-verified.
  A tested version 1 `RtlVirtualUnwind` subset is in place. Exception search now treats mapped
  executable addresses inside loaded PE images without a matching `.pdata` entry as leaf frames;
  this path is compile-checked but not runtime-verified. General SEH remains open beyond the tested
  x64 CPU-fault subset and PE32 linked-handler subsets. One direct PE32 C++ EH3 outer-frame catch and
  inner-frame destructor path is verified; other frame layouts remain untested. The bounded PE32
  `_except_handler4_common` subset is implemented and
  fixture-tested for continue-search, continue-execution, exception-code access, and normal or
  unwind-time `__finally` execution. The PE32 EH3 path also executes the fixture's compiler-generated
  nontrivial catch-copy callback. Nested/collided unwind dispositions, arbitrary catch-copy closure
  layouts, and other compiler-specific EH4 layouts remain unsupported. Across the x64 and PE32
  subsets, tested CPU-fault statuses include access violations, integer divide errors, illegal
  and privileged instructions, and software breakpoints. A browser
  fixture now executes one R32_FLOAT storage-texture dispatch, checks the UAV value by readback, and
  verifies a following draw samples it into a pixel. A follow-up headless Chromium replay fixes the group's hidden
  draw-parameter binding and captures a nonempty 1040x807 frame from the selected target with one
  dispatch, five draws, and no WebGPU validation errors. The initial, W-movement, and Tab-regeneration
  frames are saved in `docs/images/` and can be reproduced with `test/browser/target.mjs`. A separate
  normal-slice replay verified 16 Asyncify yields and resumes, three draws, one dispatch, and three
  presents without WebGPU errors; its process remained in `Starting`, and its stored route did not
  finish. Target UAV values and native comparison remain unverified; other movement directions and
  gameplay acceptance are open. See
  [`docs/TARGETS.md`](TARGETS.md) for exact traces and limits.
- The D3D11 textured fixture has Rust command-stream and WASM smoke coverage, and its tested WebGPU
  rendering states have pixel checks in `test/browser/run.mjs`. Other rendering states listed in
  `CLAUDE.md` are not pixel-verified.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
