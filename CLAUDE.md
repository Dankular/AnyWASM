# CLAUDE.md — AnyWASM

AnyWASM runs native Windows x64 executables in the browser: a PE64 loader + own x86-64 interpreter
(WASM) + host-implemented Win32/CRT libraries + Direct3D 11 translated to WebGPU. The approach is
borrowed from AnyPS5: do not emulate an OS; execute the guest code, bind imports **by name** to
reimplemented system libraries, recompile shaders, and **fail loudly** on anything missing.

## Working rules (from the owner — keep following them)
- Never speculate about what unverified code/APIs/binaries do. Verify by reading code, running it,
  or reading docs. If not verified, say so. Mark every claim as *confirmed* (source/log/test) or
  *assumed*.
- Prefer "I don't know without checking X" to a confident guess.
- Failures must be loud: unimplemented imports stop with `Stop::Unimplemented` naming `dll!Symbol`.
  Never stub silently.
- Never skip or weaken a test to get green. Verify pixel/behaviour claims with a test that can fail.
- Develop and push only on `claude/upbeat-franklin-c1vbxr`. Do not open a PR unless asked.
- Keep CI workflow sources in `ci/` mirrored under `.github/workflows/`. Pushing workflow changes
  requires a GitHub credential with `workflow` scope; if unavailable, the owner copies the files.
- Commit trailers: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` and the
  `Claude-Session:` line given by the harness.

## Layout
| Path | Purpose |
|---|---|
| `crates/anywasm-pe` | Dependency-free PE32/PE32+ parser |
| `crates/anywasm-cpu` | Paged memory, decoder, interpreter (integer, SSE–SSE4.2, x87 as f64), `DecodeCache` |
| `crates/anywasm-rt` | Loader, TEB/PEB, HLE thunks, kernel32/ntdll/msvcrt/user32/winmm/dxgi/d3d11, GPU command stream, VFS, `.awb` bundles |
| `crates/anywasm-cpu-state` | `no_std` architectural CPU state and the shared-worker ABI |
| `crates/anywasm-cpu-worker` | Worker module that runs CPU slices over shared guest memory |
| `crates/anywasm-cli` | `anywasm analyze | run | pack` |
| `crates/anywasm-wasm` | C-ABI WASM build (`aw_*` exports) |
| `web/` | `anywasm.js`, `worker.js`, `gpu.js` (WebGPU executor), `shaders.js`, vendored vkd3d-shader + Naga |
| `fixtures/` | C test programs + built `.exe` (`fixtures/build.sh`; clang/lld-link + mingw-w64) |
| `test/` | `wasm_smoke.mjs`, `bench.mjs`, `shaders.mjs`, Chromium and cross-engine Playwright browser checks |
| `tools/` | `gen_corpus.py` (diff corpus), `gen_vtables.py` (COM vtable slots from mingw headers) |
| `scripts/` | Asyncify, CPU-worker build and GTA packing scripts |
| `docs/STATUS.md`, `docs/TARGETS.md`, `docs/BROWSER_SUPPORT.md`, `docs/EVALUATION.md`, `THIRD_PARTY.md` | Verified status, real-target acceptance log, browser matrix, prior-art evaluation, licences |

## How to build and test
```
cargo test                          # includes the hardware-differential CPU suite (DIFF_FILTER / DIFF_TRIALS env)
bash web/build.sh                   # wasm build + cli + .awb demos (needs wasm32-unknown-unknown target)
node test/wasm_smoke.mjs            # real .exe files through the WASM build under Node
node test/wasm_jit_x86_movzx.mjs    # x86 MOVZX pair: interpreter/cached/emitted state differential
node test/shaders.mjs               # HLSL -> DXBC -> SPIR-V -> WGSL
node test/bench.mjs                 # interpreter throughput (instr/s) under WASM
(cd test/browser && npm i && node run.mjs)   # headless Chromium, pixel checks (uses ?test=1 mode)
node test/browser/cross-engine.mjs          # Chromium, Firefox, and WebKit core/graphics/CSP checks
bash fixtures/build.sh; node fixtures/gen_d3d_shaders.mjs   # rebuild fixtures / shader headers
node fixtures/build_bench.mjs      # rebuild the call, memory, and SSE benchmark PE fixtures (Rust Windows target)
```
Chromium path: highest `/opt/pw-browsers/chromium-NNNN/chrome-linux/chrome` (override with `CHROMIUM`).
The browser suite uses `?test=1` for direct back-buffer readback, and separately verifies the
visible 640x480 triangle canvas by screenshot without test mode. Other browser engines remain
partially exercised by `test/browser/cross-engine.mjs`; override browser paths with `CHROMIUM`,
`FIREFOX`, and `WEBKIT`, or select one engine with `BROWSER=Chromium`, `BROWSER=Firefox`, or
`BROWSER=WebKit`.

## Verified state (see docs/STATUS.md for detail)
CPU core differential-tested vs host (2,730 corpus entries); real mingw/msvcrt programs match native
output; D3D11 triangle and `d3d11_textured` pixel checks pass in headless Chromium; the selected
Diligent GLFW Demo renders a 1040x807 WebGPU frame, moves on W, and regenerates its map on Tab. The
exact checks and remaining acceptance gaps are tracked in `docs/TARGETS.md`.

---

# Outstanding tasks

Status key: **[ ]** not started · **[~]** partial. Ordered by value toward "package a real game".

## 1. Block-translating JIT to WASM  (highest priority; performance)
Why: the cached-block executor's earlier baseline measured 32.2 M guest instr/s on `compute.exe` and
26.1–38.5 M on the call-heavy, memory-heavy and SSE-float fixtures (`test/bench.mjs`, release WASM
under Node). A preliminary D3D11-only Diligent GLFW Demo run reached eight guest `Present` stops,
and a corrected aggregate counter now includes synchronous guest callbacks and yielding HLE calls.
The cached-only eight-frame sample retired 580.8M guest instructions, with 580.8M on the first
frame; forcing the current MOV emitter was about 60.5% slower on that cold pass. The native CLI
benchmark uses one 1B-instruction run slice because its synchronous callback continuation cannot
resume at a smaller configured slice boundary. The WASM/browser path uses Asyncify and has passed a
normal-slice callback-resumption replay; see `docs/TARGETS.md`. These are guest/HLE timings: AnyWASM
did not execute this target's command stream in the browser renderer. The latest same-build fixture
run used 10 paired rounds after one warm-up:
memory-heavy improved from 19.61 to 65.14 MIPS (+232.2%) and SSE-float from 26.35 to 42.74 MIPS
(+62.2%); compute regressed 68.8%, call-heavy 75.1%, and block-chain 73.6%. Every pair matched
status, exit code, stdout/stderr, and retired count. Emission remains disabled by default because
the regressions are workload-specific. These results do not establish a product performance target.
See docs/TARGETS.md for exact commands, counts, and limits.

Design (opt-in implementation; the emitted subset includes leading NOPs, selected 32-bit or
64-bit GPR MOVs and TLB-resident memory operations, 32-bit ADD, direct JMP/Jcc and register-indirect
JMP exits, vector moves, guarded packed-double ADD/MUL/SUB/DIV, and a narrow f64-model x87
register-stack subset):
- [x] **Baseline**: add a benchmark set beyond `compute.exe` (call-heavy, memory-heavy, SSE float
      loops) and record instr/s in CI (report-only; no throughput threshold yet).
- [~] **Representative-target demand**: corrected cached-only and forced-emission runs each reached
      eight Diligent GLFW Demo `Present` stops under a 1B-instruction run slice. The cached-only
      first frame retired 580,777,731 instructions and took 42.17 s; prefix 1 retired 580,776,966
      and took 67.69 s. Later frames each retired 3,200 instructions. A 10M slice can stop inside
      the target's synchronous callback and reports the non-resumable callback limit; the benchmark
      defaults to the full requested cap as one slice. Counts and Node wall times are guest/HLE-only,
      not host-rendered frame time or a product performance target.
- [x] **Block discovery**: decode and cache basic blocks keyed by guest RIP. `code_epoch` invalidates
      cached blocks on executable-memory writes and protection changes.
- [~] **WASM emission (opt-in prototype)**: Rust emits a supported prefix and JS compiles and
      instantiates it through `anywasm.run_emitted_block`. Generated modules import the runtime's
      exported memory. The JS side caches up to 256 modules and checks bytes when a process-local
      module ID is reused. `{ jitMinPrefix: 1 }` explicitly opts in; the default threshold is 65,
      above the 64-instruction block limit. Same-build medians improve on memory and SSE-float but
      regress on compute, call-heavy, and block-chain, so there is no general throughput win.
- [~] **State model (MVP)**: generated code receives a 976-byte `repr(C)` state with every current
      `Cpu` field, memory/TLB pointers, counters, and chain scratch. GPR/XMM values are block-local
      and flushed at block exit. 32-bit ADD writes arithmetic flags. Rust commits the full snapshot
      only after a valid retired count. The `jit-test-api` smoke build compares all CPU fields and
      mapped memory across compute, SSE, x87, indirect-JMP, memory, and JIT-state fixtures;
      arbitrary-program differential coverage and lazy flags remain open.
- [~] **Memory access**: selected 32-bit/64-bit MOV, memory ADD, and full-width XMM memory moves use
      inline probes of the one-entry read/write TLBs for plain, unsegmented 64-bit addresses. Accesses
      must stay within one mapped page with compatible permissions; misses, faults, and page crossings
      retry through the interpreter. Mapping/protection changes and executable-page writes invalidate
      cached translations. A direct store ends an emitted prefix. Four-page hot-access tests pass; the
      measured memory gain supports retaining this one-entry design rather than adding a multi-entry TLB.
- [~] **Exits**: immediate JMP/Jcc and register-indirect JMP update generated RIP and end the block.
      The JS trampoline chains only to registered emitted starts through a balanced RIP lookup tree.
      Unregistered or noncanonical targets return to normal lookup/fault handling, and an indirect jump
      into an HLE thunk returns to the runtime dispatcher. CALL, `Stop::*`, `skip_ret`, and
      `yield_request` remain interpreter/runtime paths.
- [~] **Fallback**: leading NOPs, selected 32-bit/64-bit GPR MOVs, 32-bit ADD immediate/register
      forms, memory MOV/ADD, direct and register-indirect exits, full-width vector moves, and guarded
      packed-double ADD/MUL/SUB/DIV are emitted. Packed-double arithmetic falls back for non-default
      MXCSR controls and zero, subnormal, infinite, or NaN operands. Unsupported SSE memory arithmetic
      and other instructions use the interpreter. 32-bit moves zero-extend; self-moves still advance
      RIP/count/TSC. Compile/trap/invalid-count failures retry from the untouched live CPU snapshot.
- [~] **Correctness checks**: Rust tests cover prefix limits, block-local register reads/writeback,
      32-bit ADD/Jcc emission, full-width vector moves, and packed-double operations. WASM smoke
      compares complete CPU snapshots and mapped guest memory across emitted, cached-only, and
      interpreter runs for compute, packed-double, x87, indirect-JMP, memory, and state fixtures.
      The fixtures check normal output/counts, known and invalid indirect targets, HLE handoff,
      fallback, and direct/conditional chaining. SSE fixtures cover finite lanes, signed zero,
      subnormal, infinity, NaN, alternate MXCSR, and interpreted memory-source arithmetic. x87
      fixtures cover FLD/FXCH/FSTP, all six selected D8 arithmetic forms, and exceptional fallback.
      Browser page and worker checks compare output/counts, including a memory-growth-after-emission
      resume. Arbitrary programs are not fully differential-tested.
- [~] **SSE/x87 in the JIT**: packed-double ADD/MUL/SUB/DIV are emitted for guarded normal finite
      register operands with default MXCSR controls; unsupported or exceptional operands fall back.
      The fixture exercises normal lanes and fallback for signed zero, subnormal, infinity, NaN,
      non-default MXCSR, and memory-form arithmetic. The x87 subset emits FLD/FXCH/FSTP register-stack
      forms and selected D8 ST arithmetic using the current f64 model; memory forms, extended precision,
      and broader exception behavior remain interpreted. Packed-single arithmetic remains interpreted.
- [x] **Browser JIT check**: headless Chromium page-realm and worker checks force the opt-in emitter
      and compare output and retired counts with cached-only execution. The page check grows WASM
      memory by one page after emission starts; the cached module resumes without another fallback.
      This JIT check passes in test/browser/run.mjs.
- [~] **Cross-browser and graphics check**: Chromium 154 and Firefox 141 run hello and the D3D11
      triangle, passing back-buffer and visible-canvas pixel checks plus the same-origin CSP JIT
      probe. The 640x480 back buffer is displayed at 640x453 when the guest client area is 624x441;
      the screenshot check follows that aspect ratio. Playwright WebKit 18.2 runs hello and the CSP
      probe but reports graphics unavailable because OffscreenCanvas transfer is missing. Full
      graphics coverage on other engines remains open. Firefox 141 passes the triangle pixel checks
      in the cross-engine suite; its graphics coverage remains limited to that fixture. WebKit 18.2
      lacks WebGPU and OffscreenCanvas transfer, so only the explicit graphics-unavailable status is
      verified there. The Chromium full browser suite passes its current graphics, input, and JIT
      assertions.

## 2. Windows runtime coverage
- [~] **Threads**: a deterministic FIFO cooperative scheduler now saves full CPU state, private
      stacks, last-error, dynamic TLS/FLS maps, and each guest thread's static PE TLS vector. It
      supports finite callbacks, quantum rotation, events, mutexes, semaphores, waits, critical
      sections, SRW locks, condition waits/wakes/timeouts, `Sleep`, and resume of blocked threads.
      Static TLS images loaded after guest-thread creation fail explicitly. Nested synchronous
      callback scheduling and alertable waits remain unsupported; this is not parallel execution.
      The target's wait result is not fully verified: an earlier bounded replay stopped at
      `SleepConditionVariableSRW`, while a fresh 30,000,000-step replay continued after that call
      into D3DCompiler and hit its cap. The latter does not prove which guest thread resumed. A
      separate 1,000,000,000-step guest trace reaches its observed MSVC C++ exception catch and later
      guest `Dispatch`, `Draw`, and `Present` calls. It does not verify a host renderer or frame.
- [~] **SEH / C++ exceptions**: PE32+ `.pdata` entries are parsed and validated;
      `RtlLookupFunctionEntry` and a tested `RtlVirtualUnwind` subset support leaf frames, common
      version 1 unwind opcodes, handler metadata, and chained records. `RaiseException` now searches
      ordinary x64 records through registered vectored exception handlers and `__C_specific_handler`
      scope tables, including filters, catch-all scopes, continuation, and termination handlers;
      target unwind uses the existing second-pass `RtlUnwindEx` path. Synthetic regressions cover
      handler ordering and scope selection, while the selected MSVC C++ catch remains verified by a
      bounded target replay. The source now also routes continuable VEH/SEH exceptions through
      vectored-continue callbacks and invokes the registered top-level filter after an exhausted
      search; synthetic regressions cover VCH ordering, ignored callback returns, filter replacement,
      and filter-edited context resumption. Selected x64 access violations, integer divide errors,
      illegal instructions, privileged instructions, and software breakpoints now enter the
      dispatcher; guest VEH tests cover exception records, CONTEXT.Rip edits, and MXCSR
      round-tripping. The first PE32 path serializes x86 EXCEPTION_RECORD/CONTEXT data, invokes
      vectored handlers and linked NT_TIB exception registrations, resumes edited contexts, and
      applies RaiseException's x86 stdcall cleanup. Synthetic guest execution covers selected x86
      illegal, privileged, divide, and breakpoint exceptions; a scheduler regression preserves each
      thread's NT_TIB.ExceptionList. The MSVC PE32 C fixture passes nested exceptions escaping a
      filter and exceptions raised during `__finally` natively, through the CLI, and in a browser
      package. A synthetic handler regression covers disposition 2 setting `EXCEPTION_NESTED_CALL`.
      Full nested-frame bookkeeping, explicit collided-unwind disposition handling, compiler-specific
      C/C++ handlers, other CPU exits, and version 2 unwind data remain open.
- [~] **More of kernel32/ntdll/user32/shlwapi**: target-grounded work adds
      `VerifyVersionInfoA/W`, `PathFileExistsA/W`, and cooperative wait support alongside the
      earlier `InitializeSListHead`, `GetCapture`, `IsWindowUnicode`, `GetDlgItem`, and
      `EnumDisplaySettingsW`, plus `advapi32!SystemFunction036` and `RtlPcToFileHeader`. The
      selected target passes version checks, creates virtual window and D3D objects, and resolves
      `RenderStates.json`. The 30,000,000-step trace proceeds after `SleepConditionVariableSRW` but
      does not prove that the blocked thread resumed. Separate higher-cap traces catch its MSVC C++
      `std::runtime_error`, probe Direct3D12 registry paths, and reach guest D3D11 calls. Host
      window/device behavior and actual rendering remain unverified; see `docs/TARGETS.md`.
- [~] **GDI / gdi32**: virtual display DC creation and basic display capabilities are available;
      drawing, common controls, message boxes, and clipboard remain open as needed by real programs.
- [ ] **Audio**: winmm waveOut, DirectSound, XAudio2, WASAPI → WebAudio (AudioWorklet).
- [~] **Input devices**: `test/browser/run.mjs` verifies Playwright `WM_KEYDOWN` and
      `WM_LBUTTONDOWN`, maps a 256x192 CSS content area to the 1024x768 Win32 client area inside
      the 1040x807 guest window, and exercises `TrackMouseEvent(TME_LEAVE)`/`WM_MOUSELEAVE`.
      `test/browser/target.mjs`
      verifies W/S/A/D movement, cursor-aimed movement in all four directions, a flashlight response,
      and Tab map regeneration in the selected Diligent maze; browser input skips hidden top-level
      helper windows. The positive flashlight measurement is from a seeded build; the random Release
      bundle can miss the current bright-pixel threshold when walls occlude the beam. Make that check
      repeatable with a seeded bundle or a verified clear ray before using it as a general target gate.
      Add XInput
      (Gamepad API), DirectInput, raw input, cursor capture/pointer lock.
- [~] **Registry**: `advapi32!RegOpenKeyExA` recognizes predefined roots, returns the root for an
      empty subkey, and reports missing subkeys while no registry data store exists. Target probes
      `HKCU` and `HKLM` at `Software\Microsoft\Direct3D\Direct3D12`; query/create/write APIs and
      populated registry data remain open. Other outstanding areas include COM beyond D3D/DXGI,
      shell/known-folders, networking (winsock → WebSocket/fetch relay), timer delivery and
      QueryPerformanceCounter accuracy under other scheduling patterns, file dialogs, persistent VFS
      (IndexedDB/OPFS) — verify what is already implemented before starting each. `SetTimer` and
      `KillTimer` currently only store/remove registrations; no `WM_TIMER` messages or `TimerProc`
      callbacks are scheduled. QPC uses virtual ticks derived from retired guest instructions plus
      host time injected after browser frame yields. Browser elapsed values are converted from 100 ns
      units to the internal 10 ns ticks, matching the configured 10 MHz QPC; process and selected
      target regressions cover this frame-yield path. Accuracy for other scheduling patterns remains
      open.
- [~] **32-bit PE (WoW64-style)**: x86-32 decode/execution, PE32 image mapping, 32-bit TEB/PEB
      setup, and stdcall stack cleanup are in place. A synthetic PE32 entry plus `SetLastError`
      import-table call now verifies IAT binding, guest execution, and TEB last-error state;
      another synthetic PE32 image verifies process TLS callbacks and per-thread static TLS through
      the 32-bit TEB vector. Import-heavy DLLs, broad Win32 API coverage, and a real 32-bit program
      remain unverified.
      This is still a large target-compatibility task.
- [~] **DLL loading**: the selected Diligent target maps and executes code from
      `GraphicsEngineD3D11_64r.dll` and `D3Dcompiler_47.dll`; their nonzero entry points ran through
      `DllMain` successfully in the bounded target trace. Initial loading and `LoadLibrary` now run
      bundled DLL process-attach calls dependency-first; synthetic nested-DLL coverage exercises
      both paths. PE TLS callbacks now run in image order before `DllMain`; EXE callbacks run after
      DLL initialization and before the EXE entry point. Synthetic fixtures cover callback ordering,
      attach arguments, and EXE startup. `Archiver_64r.dll` has an entry point but was not observed
      loading. Static TLS data is initialized. Named forwarded exports are resolved
      opportunistically, failed names are omitted, and ordinal forwarders are dropped. The four
      observed target images do not contain TLS callbacks or forwarded exports, so target coverage
      for those paths remains unverified.
- [x] **Packer/analyzer**: `--dir` recursively adds assets with preserved relative subpaths and
      `--args` embeds the caller's command line. `run` and `pack` accept `--cwd`; the process stores
      a guest current directory, resolves relative Win32 and CRT file paths against it, and `.awb`
      bundles persist it. Older bundles default to `C:\`. `SetCurrentDirectoryA/W`,
      `GetCurrentDirectoryA/W`, `GetFullPathNameA/W`, and relative file operations have guest
      regression coverage. Directories are inferred from VFS file paths or tracked when created;
      `CreateDirectoryA/W` support empty directories and require an existing parent, with guest
      regression coverage. CLI `run` gives synchronous guest callbacks the full remaining
      `--max-steps` budget and accounts for retired instructions across frame stops; its regression
      callback runs for more than the former 2,000,000-instruction internal slice.
      `SetFileAttributesA/W` remain unimplemented.
      `analyze --json` now adds `schemaVersion: 1` while preserving existing top-level fields. Its
      `preflight` summary separates known blockers from potentially unused missing imports and
      explicitly does not guarantee runtime success. The JSON serializer escapes control characters.

## 3. Graphics
- [~] **D3D11 verification gaps**: browser pixel/readback checks cover source/destination alpha
      and color, source-alpha-saturate, and blend-constant factors with inverse forms; they also cover
      reverse-subtract, add, subtract, min, and max blend operations;
      texture, vertex, index, structured, raw, typed, and constant-buffer `CopyResource`; Texture1D
      and Texture1DArray sampling lower to 2D backing textures, with a 3-mip/2-slice array pixel-tested
      at layer 0/mip 1 and layer 1/mip 2; texture and buffer `CopySubresourceRegion`, including a
      partial RG8_UNORM box, partial R16_UNORM/SNORM copies with copied and untouched words retained
      through staging readback, and aligned BC1/BC2/BC3/BC4/BC5/BC6H/BC7 blocks with untouched neighbouring data,
      plus `UpdateSubresource`;
      DXBC clip/discard; 2D and 3D `GenerateMips`;
      fractional-LOD linear filtering; repeat/mirror/clamp/MIRROR_ONCE sampler address pixels;
      BORDER verifies configured color for point and linear taps in 2D RGBA8 Sample, SampleLevel,
      SampleGrad, and SampleBias paths, plus 2D-array and 3D RGBA8 SampleLevel paths. D32_FLOAT
      comparison BORDER verifies all eight comparison functions for fully outside point-filtered
      SampleCmpLevelZero taps and for edge-straddling and fully outside linearly filtered SampleCmp
      footprints; R8_UNORM, RG8_UNORM, A8_UNORM, B5G6R5_UNORM, BC1, and BC4 BORDER tests verify SRV
      channel defaults; BC2/BC3 alpha and BC5 RG defaults are also pixel-checked. 2D and 2D-array
      RGBA8 Gather verify edge-straddling and fully outside taps,
      including an explicit nonzero offset; D32_FLOAT GatherCmp and R8_UNORM missing-alpha Gather
      defaults are also pixel-checked. An 8x anisotropic Texture2D SampleLevel outside sample and an
      in-range SampleGrad footprint crossing the right edge are pixel-checked.
      An 8x anisotropic Texture2D comparison SampleCmp and Texture2DArray implicit Sample now have
      edge-crossing BORDER pixels. Texture2DArray SampleGrad and zero-bias SampleBias each return
      `[0, 151, 104, 255]` for an explicit 8x footprint crossing the right edge on layer 1. On R8_UNORM
      and RG8_UNORM Texture2DArray views, 8x anisotropic Sample, SampleGrad, and zero-bias SampleBias
      each return `[76, 0, 0, 255]` and `[38, 114, 0, 255]` at that edge, verifying channel defaults.
      An 8x anisotropic BC4_UNORM Texture2D Sample, SampleBias, and SampleGrad also verify a constant
      interior and channel-correct BORDER blends across the right edge.
      Texture3D R8_UNORM BORDER sampling returns `[0, 0, 0, 255]` outside and `[115, 0, 0, 255]` at a
      3D edge footprint, verifying channel defaults and edge weighting.
      Texture3D RG8_UNORM BORDER sampling returns `[0, 0, 0, 255]` outside and `[115, 58, 0, 255]`
      at the same edge footprint, verifying both normalized channels and missing-channel defaults.
      Texture3D R16_UNORM and R16_SNORM samples match `[115, 0, 0, 255]` at that edge within 4
      channel values, verifying normalized 16-bit 3D texture backing and channel defaults.
      Texture3D R16_FLOAT samples decode half-float 0.5 and return `[0, 0, 0, 255]` outside and
      `[115, 0, 0, 255]` at the edge within 4 channel values.
      Texture1DArray R8_UNORM sampling returns `[128, 0, 0, 255]` from mip 1/layer 0 and
      `[64, 0, 0, 255]` from mip 2/layer 1; the dimension-query pixels match the RGBA8 array case.
      TextureCube SRV selects the expected
      color for all six
      signed-axis directions and linearly blends taps across +X/+Y, +X/+Z, and +X/-Y boundaries to
      `[127, 0, 127, 255]`, `[255, 0, 127, 255]`, and `[255, 127, 0, 255]` respectively. vkd3d
      rejects the documented float2 DDX/DDY arguments for
      Texture2DArray.SampleGrad with E5002 ([Microsoft HLSL signature](https://learn.microsoft.com/en-us/windows/win32/direct3dhlsl/dx-graphics-hlsl-to-samplegrad));
      anisotropic Texture2D comparison SampleCmpLevelZero also returns `[110, 0, 0, 255]` for the
      same edge-crossing LESS footprint. vkd3d rejects the documented float2 Texture2DArray.SampleGrad
      gradients with E5002, but accepts float3 gradients and lowers their xy components to vec2; the
      pixel regression uses this compiler workaround. `TextureCubeArray.SampleLevel` now validates in
      the pinned Naga bridge with `CUBE_ARRAY_TEXTURES` enabled. Chromium samples indices 0 and 1 from
      a two-cube view beginning at face 6, verifies the +X/+Y linear seam, and reads the expected
      width, height, cube count, and mip count with no WebGPU validation errors. A selected RGBA16_FLOAT
      cube-array view returns its two expected half-float colors; an R8_UNORM view beginning at face 6
      returns `[128, 0, 0, 255]` and `[64, 0, 0, 255]` for cube indices 0 and 1 and reports its view
      dimensions, with no WebGPU validation errors. An R10G10B10A2_UNORM cube-array view beginning at
      face 6 returns `[64, 128, 192, 255]` and `[192, 64, 128, 255]` for the selected cubes and reports
      two cubes and one mip with no WebGPU validation errors. An R11G11B10_FLOAT cube-array view also
      decodes packed colors `[128, 64, 255, 255]` and `[255, 128, 64, 255]` for the two selected cubes,
      reports two cubes and one mip, and produces no WebGPU validation errors. An RGBA8_SRGB cube-array
      view beginning at face 6
      selects both cubes and returns linearized colors [55, 13, 134, 255] and [13, 134, 55, 128],
      with alpha preserved and no WebGPU validation errors. The bundled vkd3d compiler reports E5005 for
      `SampleCmpBias`/`SampleCmpGrad` under `ps_5_0` and E5033 (unknown target profile) for `ps_6_0`
      and `ps_6_8`, leaving those methods untestable with this compiler. `Texture1DArray` and
      `Texture2DArray` `GetDimensions` now pass through Naga's image
      size and layer queries; shader tests and Chromium pixels verify mip 0/1 widths, two elements,
      and three levels. D32_FLOAT, D16_UNORM, D24_UNORM_S8, and D32_FLOAT_S8X24
      `TextureCubeArray.SampleCmp` and `SampleCmpLevelZero` with LESS compare two-cube views beginning
      at face 6; Chromium reads false for cube 0 and true for cube 1 in all four formats and sampling
      methods, with no WebGPU validation errors. Combined depth/stencil SRVs use the aspect-specific
      WebGPU depth format. Cube-array
      `GetDimensions` also returns the expected 4x4 mip-0 and 2x2 mip-1 sizes for the two-cube, two-mip
      view. The tested array/3D formats include RGBA8, R8_UNORM, RG8_UNORM, R16_FLOAT, R16_UNORM, and
      R16_SNORM;
      the sRGB and R10G10B10A2_UNORM/R11G11B10_FLOAT cube-array samples are also verified. Other sampled dimensions/
      formats, anisotropic footprints, cube-array SampleCmpBias/SampleCmpGrad, and array/3D format cases
      remain open. An 8x anisotropic BC4_UNORM Texture2D BORDER test covers Sample, SampleBias, and
      SampleGrad with constant interior values and channel-correct edge blends.
      D3D11 `CheckMultisampleQualityLevels` reports one 4x level and `CreateTexture2D` forwards the
      sample count for `R10G10B10A2_UNORM` and `R11G11B10_FLOAT`; a Rust table test enumerates all ten
      advertised color/depth formats at 2x, 4x, and 8x and checks representative unsupported formats.
      Chromium verifies the resolve pixels. Chromium also tests near/far depth
      with 4x D32_FLOAT, D24S8, and D16_UNORM attachments and resolves the retained green draw to RGBA8.
      A two-layer 4x RGBA8 render target resolves only layer 1 and preserves layer 0; the WebGPU
      executor backs each D3D11 MSAA array layer with a separate 2D multisampled texture.
      A combined 4x D24S8/alpha-to-coverage draw resolves near red and farther green samples without
      the clear blue channel. `Map(READ)` executes older
      commands already transferred to the host-visible GPU queue before its current HLE batch,
      preserving order across shared CPU slices; the default shared-worker triangle fixture verifies
      the staging values. Other format/resolve combinations and render-state combinations remain open.
      Staging `Map(READ)`
      verifies RGBA8 back-buffer pixels, copied buffer values, two-row R32_FLOAT texels and
      R32_UINT/SINT words,
      R32G32_FLOAT two-channel texels with row pitches, R32G32_UINT/SINT packed channel words,
      RGBA32_FLOAT four-channel rows and RGBA32_UINT/SINT packed channel words with 32-byte rows,
      R16 and RG16 UNORM/SNORM/UINT/SINT words, RG16_FLOAT rows, and RGBA16_UNORM/SNORM/UINT/SINT
      words; R16_UNORM/SNORM preserve adjacent guest words through upload, full-resource copy, and
      staging readback. Their sampling uses R32_FLOAT backing when `float32-filterable` is available,
      with half-float fallback otherwise. The suite also checks packed
      RGBA16_FLOAT and R16_FLOAT values,
      raw BGRA8_UNORM bytes; R10G10B10A2_UNORM/UINT and R11G11B10_FLOAT packed words,
      encoded RGBA8_SRGB and BGRA8_SRGB bytes; RGBA8_UNORM and R8/RG8/RGBA8 SNORM/UINT/SINT plus
      A8_UNORM raw bytes; R8G8_UNORM two-channel texels; odd-width R8_UNORM row packing plus
      mip/array subresource selection; depth-only D16_UNORM and D32_FLOAT values; D32_FLOAT_S8X24
      depth/stencil/padding bytes when the optional WebGPU format is available; and Texture3D
      R32_FLOAT row/depth
      pitches; D24_UNORM_S8 depth/stencil bytes by sampling and quantizing depth to 24 bits; and
      B5G6R5, B5G5R5A1, and B4G4R4A4 packed words via RGBA16_FLOAT backing. WebGPU's depth24plus
      representation may use 24- or 32-bit depth ([format rules](https://www.w3.org/TR/webgpu/#depth-stencil-formats)).
      Comparison samplers pixel-test all eight
      comparison functions against depth values on both sides of the reference; depth-state
      comparisons pixel-test all eight functions at equality. Stencil comparisons cover all eight
      functions with stored values below, equal to, and above the reference. Stencil pass operations
      cover keep, zero, replace, both saturating operations, invert, and both wrapping operations,
      including 8-bit boundaries. Other staging formats and subresource layouts, other format/resolve
      combinations, and more render-state combinations remain open.
- [~] **Compute shader target path**: D3D11 HLE advertises typed UAV support for R32_FLOAT,
      creates texture UAVs, records CS UAV bindings and initial counts, records D3D11.1 constant
      buffer ranges, and emits dispatch commands. The WebGPU executor binds the selected
      single-float R32_FLOAT storage-texture case and executes compute pipelines. A browser fixture
      verifies a 4x4 dispatch by reading back the UAV and sampling it in a draw. The selected
      Diligent demo's SDF compute shader now translates and passes WebGPU module/pipeline creation;
      a browser replay accepted its dispatch and draw pipelines. Naga's hidden group-3 draw-parameter
      uniform is bound per draw at binding 0; D3D UAV register `u#` maps to binding `# + 1`, and a
      Chromium pixel regression exercises distinct nonzero `baseVertex` and `firstInstance` values.
      The selected target's 1040x807 readback is saved in
      [`docs/images/glfwdemo-first-frame.png`](docs/images/glfwdemo-first-frame.png); SDF maze pixels
      are visible with no WebGPU validation errors. A deterministic seed-1 native D3D11 capture has
      a bounded visual comparison in `docs/TARGETS.md`. Its 128x128 R32_FLOAT SDF UAV is independently
      read back with all finite values inside the shader's ±4 bound, and a controlled seed-1 browser
      replay traverses the maze to its teleport; other UAV formats, pixel equivalence, graphics
      states, other gameplay states, and broad game acceptance remain unverified. The random-map
      flashlight probe now selects an unobstructed SDF ray to a wall within the light range and
      checks local pixels; two fresh Release-bundle launches passed. Target-driven Tab
      map reset is pixel-checked after adding D3D11 `Flush1` event signaling and event-query
      completion; `Begin` does not reset an event query. Unsupported query and predicate types now
      return `E_NOTIMPL`, while invalid types
      return `E_INVALIDARG`. D3D11.2 `OPTIONS1` reports tiled-resource tier 0 and false for its
      other capabilities in a 16-byte regression. GPU counters and predication remain unsupported. Browser fixtures
      cover every typed UAV texture format currently advertised by the HLE across R32, RG32, RGBA8,
      RGBA16, and RGBA32 formats, plus structured/raw/typed buffer UAV ranges and a 3D texture UAV.
      Deferred contexts and command lists have Rust coverage for creation/type/flags, recording,
      finish/reset, execute ordering, and state restoration. UAV formats outside the advertised set,
      geometry/hull/domain shaders, additional staging layouts, `SV_` system values beyond those tested,
      tiled resources, and honest feature-level reporting remain open.
- [~] **Shader toolchain**: broader DXBC coverage in vkd3d-shader → SPIR-V → Naga → WGSL; a captured
      Diligent GLFW Demo SDF shader now has a regression for its non-finite integer-register bit
      patterns and translates through a runtime bitcast. The target's draw-parameter uniform and UAV
      binding collision is fixed and pixel-tested. Diligent DrawMap vertex/pixel shader fixtures
      cover constant buffers, texture sampling, and ray loops; Chromium validates their WebGPU
      pipeline and checks ray-marching response to clear-space versus wall SDF samples. More failing
      real-world shaders, shader/pipeline caching (IndexedDB), and async pipeline creation to avoid
      stalls remain open.
- [~] **Canvas blit pixels**: Chromium and Firefox screenshots verify the visible D3D11 triangle's
      clear colour and interpolated centre pixel. Chromium also checks the textured fixture's
      blended quad and sampled texture colours. Chromium additionally verifies visible swapchain
      pixels at 32x24 and 96x64 after ResizeBuffers, plus DPR 2 presentation with a 640x480 guest
      backing store. Other render paths and browser engines still need coverage.
- [~] **Additional graphics APIs**: D3D9 has pixel-tested stage-0 fixed-function operations for
      selected FVF layouts, SELECTARG1/SELECTARG2, MODULATE/MODULATE2X, ADD/SUBTRACT/ADDSMOOTH,
      signed-add and modulate-add, DOTPRODUCT3, MULTIPLYADD/LERP with COLORARG0/ALPHAARG0, and
      color/alpha blend operations. Triangle fans, alpha blending, and every alpha compare function
      at equality are also checked. Two-stage PREMODULATE is pixel-tested: stage 0 passes arg1,
      and stage 1 SELECTARG1 CURRENT is multiplied by its texture per channel. D3D9 HLE rejects
      stage-1 PREMODULATE, bump-environment color/alpha operations, and bump matrix states 7–10 on
      both stages without changing state or emitting GPU commands. Broader multi-stage cascades,
      programmable shaders, and broad device/state coverage remain open. D3D12,
      DXGI flip-model/fullscreen/resize/vsync
      semantics, OpenGL (WGL), and Vulkan remain open. Prior art (WineBrowser: D3D9/12→WebGPU, MIT)
      is documented in `docs/EVALUATION.md` — review licence/architecture before borrowing.
- [~] **Frame pacing**: after each `Stop::Frame`, the worker advances the virtual guest clock by the
      measured time spent waiting for browser `requestAnimationFrame`, converting elapsed 100 ns
      units to internal 10 ns ticks. A process regression and the selected target's movement replay
      verify this scale against its 10 MHz `QueryPerformanceCounter`. `Present` interval semantics,
      scheduling under JIT and offscreen canvas resize after transfer remain open.

## 4. CPU correctness
- [~] **AVX instruction coverage**: selected VEX forms now cover target-used VMOVUPS/APS,
      VMOVDQA/DQU, VMOVSS/SD, VPXOR/VXORPS, VADDSS/VDIVSS, VCOMISS/VUCOMISS, VROUNDSS,
      VCVTSI2SS/VCVTTSS2SI, VEXTRACTF128, VZEROUPPER, ANDN, SHLX, SHRX, BZHI, VPBROADCASTD,
      VPADDD, VMOVDQU, VMOVQ, VPEXTRD/Q, VCMPEQSS, VBLENDVPS, VPERMILPS/PD, VMOVD, and BLSI.
      Target-grounded packed-single VADDPS/VMULPS now support XMM and YMM forms; Tutorial03's
      observed sequence exercises the XMM forms beside VPERMILPS.
      These are exact target-grounded forms, not broad AVX/AVX2 support. BMI1 and BMI2 instruction
      families are implemented and advertised through CPUID. Remaining AVX/AVX2/FMA and
      AES/PCLMUL/MMX instructions are open.
- [ ] **x87 precision**: currently f64, not 80-bit; implement soft-float 80-bit if a target needs it.
- [x] Document `rcpps`/`rsqrtps` approximation behavior. Intel and AMD specify a maximum relative
      error of `1.5 × 2^-12`, without fixing exact result bits (confirmed by the linked manuals);
      inference: valid estimates may vary by processor. The interpreter deliberately computes exact
      scalar reciprocals instead, and these instructions remain excluded from bit-exact host
      differential checks. See `docs/STATUS.md`.
- [~] Extend the diff harness to control flow (jcc/call/ret/loop/string ops with REP), privileged-ish
      instructions used by user code (`rdtsc`, `cpuid`, `xgetbv`, `syscall` stubs), and multi-
      instruction sequences. Added LOOP/Jcc, direct CALL/RET, and REP string cases; portable REP
      checks cover zero count, DF direction, operand widths 8/16/32/64, a 32-bit address-size
      override, and early termination. Focused deterministic contracts now cover CPUID vendor and
      feature outputs, XGETBV, RDTSC/RDTSCP counters, and the syscall stub; these are interpreter
      regressions rather than host-differential cases. The target BZHI memory form now runs through
      the Linux host differential corpus with deterministic index cases 0, 1, 31, 32, 33, 0x101,
      and 255; AF/PF are masked because they are undefined. Linux-only byte-sized host differential
      cases remain unrun locally. Undefined flags/vendor-specific behaviour must be excluded and
      documented (e.g. 16-bit shld/shrd count>16).
- [x] Anti-debug/anti-cheat/DRM: documented as unsupported; revisit only if a representative target
      requires it.

## 5. Tooling, CI, repo hygiene
- [x] **Copy `ci/web.yml` to `.github/workflows/`** (mirrored as `web.yml`; requires `workflow` scope).
- [ ] **CI is red.** `Rust` and `WASM` workflows pass through `bd6f0bd` (and `631ba66` was not
      checked at that time) but fail on every push inspected from 2026-10-12 to 2026-10-13, including
      branch head `ef301dd`. Confirmed from the `Rust` log for `4caaf0c`:
      `E0432 unresolved import crate::mem` at `anywasm-cpu/src/x87.rs:521` while compiling
      `anywasm-cpu-state` lib tests; it builds locally with `cargo test -p anywasm-cpu-state
      --no-run`, so root cause is **unknown**. `WASM` workflow failure reason not yet read.
- [x] Removed the tracked two-line root `out.txt` added in `800f4e1` and deleted in `1c8246c`.
      `fixtures/crt_test.c` separately writes a guest `out.txt` during its CRT test.
- [x] Silence/resolve compiler warnings (e.g. unused `ERROR_ACCESS_DENIED` in `kernel32.rs`).
- [x] Add Rust-side command-stream tests for both D3D11 fixtures in `tests/d3d11_stream.rs` and
      add the textured exe to `test/wasm_smoke.mjs`.
- [x] Add WASM smoke regressions for condition-variable thread resumption and per-thread static PE
      TLS isolation.
- [x] Performance dashboard: record bench numbers per commit in CI (non-gating at first).
- [x] Task list housekeeping: current open work is queued in [JIT](https://github.com/Dankular/AnyWASM/issues/1), [runtime](https://github.com/Dankular/AnyWASM/issues/2), [graphics](https://github.com/Dankular/AnyWASM/issues/3), [CPU](https://github.com/Dankular/AnyWASM/issues/4), and [productization](https://github.com/Dankular/AnyWASM/issues/5); keep their open checklists in sync with this file. Historical #5/#11/#6 references cannot be mapped because the prior issue list was empty and the old notes' broader status cannot be recovered.

## 6. Productisation (after a real target runs)
- [~] Analyze three Win64 Diligent samples (HelloTriangle, Texturing, Asteroids) and record their
      import blockers in `docs/TARGETS.md`. Historical D3D11 traces stopped at the MSVCP140
      `basic_ios<char>` constructor; HLE support and a local-runtime probe now advance past it.
      A package-aware browser capture of Tutorial03 shows the textured Diligent cube after 59 draws
      and 30 presents, with two pipelines and no WebGPU validation errors. A fresh current-source
      HLE-only browser replay without local MSVC/UCRT DLLs recorded 2,759 draws, 1,380 presents,
      5,519 passes, two pipelines, 16 Asyncify yield/resume pairs, and no WebGPU validation errors; it
      remains in `Starting` at the bounded report timeout, without terminal process status. Its only
      404 was the browser's `/favicon.ico` request. The current static import report lists 205
      implemented / 46 missing, including 26 of 65 MSVCP140 imports still missing; see
      `docs/target-imports/tutorial03-current-source.json`. Neither report shows the sample's later
      execution. HLE-only Tutorial01
      now renders its colored triangle without local MSVC/UCRT DLLs at the standard 250,000-step
      worker slice (659 draws, 330 presents, two pipelines, 16 Asyncify yields/resumes, no WebGPU
      validation errors), but remains in `Starting`. Complete runtime support and redistribution of
      local runtime DLLs remain open.
      Asteroids remains static-analysis only and needs a separate Intel-origin code/assets
      redistribution review.
      The Apache-2.0 GLFW Demo maze is now the selected game candidate. A separate D3D11-only `/MT`
      build succeeded; the current
      package-aware analyzer reports 84 missing imports across its EXE and three DLLs. A prior
      status note listed 96, which the current build does not reproduce; see `docs/TARGETS.md` for
      the `--dir` distinction and per-image report. An older dynamic-runtime snapshot reported 349
      with an older analyzer. The browser test now verifies all
      four WASD directions and cursor-aimed movement in all four directions, plus the flashlight's
      visible response to a held left click;
      guest pointer coordinates account for canvas borders and the CSS-to-window scale, and
      `TrackMouseEvent(TME_LEAVE)` produces `WM_MOUSELEAVE`. Its
      `MultiByteToWideChar` and `LCMapStringEx` size-query faults are fixed and covered. Scheduler
      regressions cover condition-variable wake/resume. An earlier bounded run stopped at
      `SleepConditionVariableSRW` because its blocked guest continuation could not be resumed; a
      fresh 30,000,000-step CLI replay reaches that call, continues after the wait, and hits its step
      cap in D3DCompiler. A separate high-cap guest trace reached `Present` calls; those guest-only
      runs did not verify a host window or renderer. After target-grounded static TLS and VEXTRACTF128 support,
      a bounded trace first reached guest `ExitProcess(3)`. Target-grounded MSVC C++
      exception search and second-pass unwind now reach the
      catch. After the observed `HKCU`/`HKLM` Direct3D 12 registry probes, a 1,000,000,000-step
      traced replay reached `CreateComputeShader`, `CSSetUnorderedAccessViews`, `Dispatch`, then
      guest `Draw` and `IDXGISwapChain2::Present` calls. Target-grounded VEX/BMI handlers and D3D11
      UAV/constant-buffer-range command records moved execution through these paths. The replay was
      stopped after confirming repeated guest presents. Headless Chromium now replays the target's
      compute and graphics commands through WebGPU with no validation errors and captures visible
      1040x807 SDF maze pixels; `test/browser/target.mjs` reproduces this against a caller-supplied
      bundle and saves PNGs for the initial frame, W movement, and Tab map regeneration. Browser
      input is routed past GLFW's hidden helper window; D3D11 `Flush1` signals the optional guest idle
      event, and event-query `GetData` completes in the ordered command-stream model. The seed-1
      target verifies W/S/A/D, cursor-aimed movement in all directions, and flashlight illumination;
      measurements are saved in
      `docs/target-data/glfwdemo-seed1-input-directions.json` and
      `docs/target-data/glfwdemo-seed1-mouse-aim.json` and
      `docs/target-data/glfwdemo-seed1-flashlight.json`. A deterministic seed-1 native D3D11 capture
      is visually compared with the browser frame in `docs/TARGETS.md`;
      `--check-uav` reads the target's 128x128 R32_FLOAT SDF and records finite-value, sign, and range
      checks there. These do not establish pixel equivalence or end-to-end rendering correctness. A
      stored seed-1 maze traversal predates the 100 ns-to-10 ns frame-time correction and needs a
      rerun before its route timing is treated as current. The general Release bundle uses
      `std::random_device` for map generation; its flashlight threshold
      varies between launches and failed once in an unseeded replay. Keep the stored flashlight claim
      scoped to its seeded build and aim direction; see `docs/TARGETS.md`. The normal
      250,000-instruction browser worker slice resumed the target callback 16 times in a
      bounded replay (three draws, one dispatch, three presents); its page remained in `Starting`
      and the recorded maze route did not finish. One pre-correction controlled seed-1 maze traversal
      reached the teleport; rerun it with the corrected frame clock.
      Broad gameplay and end-to-end game acceptance remain open; see `docs/TARGETS.md` for exact
      evidence and limits.
- [~] Packaging flow: the browser now shows the first reached unimplemented import in an accessible
      alert with the exact runtime message and `anywasm analyze` guidance; verified with the
      `unimpl.exe` fixture. It also shows phase-based load progress. The new browser package library
      reads static catalogs, resolves exact-version dependencies, stores SHA-256 checked AWB bundles
      in IndexedDB, and keeps AGP data remote; a local GTA catalog install and launch reached guest
      rendering. Invalid dependency/error cases, reload/removal behavior, per-chunk integrity, mod
      overlays, and single-file/static-site export remain open. The bounded launch did not establish
      gameplay acceptance; see `docs/TARGETS.md`.
- [~] Browser support matrix: Chromium page/worker/input and WebGPU readback behavior, Firefox
      hello plus version-specific graphics availability, and WebKit hello plus its OffscreenCanvas
      limitation are recorded in `docs/BROWSER_SUPPORT.md`. Windows Firefox 141.0 exits the triangle
      fixture after three frames but reports a missing-bind-group validation error and renders black;
      Firefox 140.0.2 lacks WebGPU. The D3D11 triangle canvas blit is screenshot-checked only in
      Chromium. GTA AGP range streaming has been exercised; 4 GB / `memory64` behavior, a comparable
      performance result for the latest range scheduler, and per-chunk integrity remain open. The
      `wasm32-unknown-unknown` browser path buffers the full bundle, copies it into WASM memory, then
      duplicates each file into the VFS; this source audit is not a peak-memory measurement.
- [~] Licensing review of vendored vkd3d-shader (LGPL) and Naga: technical inventory and pre-ship
      obligations are in `THIRD_PARTY.md`; legal review of the packaged combined work is still required.
- [x] Legal/scope statement: user supplies their own legally obtained binaries.
