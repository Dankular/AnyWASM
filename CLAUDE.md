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
| `crates/anywasm-cli` | `anywasm analyze | run | pack` |
| `crates/anywasm-wasm` | C-ABI WASM build (`aw_*` exports) |
| `web/` | `anywasm.js`, `worker.js`, `gpu.js` (WebGPU executor), `shaders.js`, vendored vkd3d-shader + Naga |
| `fixtures/` | C test programs + built `.exe` (`fixtures/build.sh`; clang/lld-link + mingw-w64) |
| `test/` | `wasm_smoke.mjs`, `bench.mjs`, `shaders.mjs`, Chromium and cross-engine Playwright browser checks |
| `tools/` | `gen_corpus.py` (diff corpus), `gen_vtables.py` (COM vtable slots from mingw headers) |
| `docs/STATUS.md`, `docs/EVALUATION.md`, `THIRD_PARTY.md` | Honest status, prior-art evaluation, licences |

## How to build and test
```
cargo test                          # includes the hardware-differential CPU suite (DIFF_FILTER / DIFF_TRIALS env)
bash web/build.sh                   # wasm build + cli + .awb demos (needs wasm32-unknown-unknown target)
node test/wasm_smoke.mjs            # real .exe files through the WASM build under Node
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
frame; forcing the current MOV emitter was about 60.5% slower on that cold pass. The benchmark uses
one 1B-instruction run slice because synchronous callbacks cannot yet suspend at a smaller slice
boundary. These are guest/HLE timings: AnyWASM did not execute this target's command stream in the
browser renderer. The latest same-build fixture run used 10 paired rounds after one warm-up:
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
      version 1 unwind opcodes, handler metadata, and chained records. Target-grounded frame search,
      language-handler invocation, second-pass unwind, `RtlUnwindEx`, and the selected MSVC C++ catch
      are verified with a synthetic regression and bounded target replay. Version 2 unwind records,
      general SEH behavior, vectored handlers, and guest faults → exceptions remain open.
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
      `WM_LBUTTONDOWN`, scales mouse coordinates from the CSS client area to the 1040x807 guest
      window, and exercises `TrackMouseEvent(TME_LEAVE)`/`WM_MOUSELEAVE`. `test/browser/target.mjs`
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
      import-heavy DLLs, broad Win32 API coverage, and a real 32-bit program remain unverified.
      This is still a large target-compatibility task.
- [~] **DLL loading**: the selected Diligent target maps and executes code from
      `GraphicsEngineD3D11_64r.dll` and `D3Dcompiler_47.dll`; their nonzero entry points ran through
      `DllMain` successfully in the bounded target trace. `Archiver_64r.dll` has an entry point but
      was not observed loading. All four target images have TLS directories but no TLS callbacks,
      and none has forwarded exports, so those behaviours are not verified. Source inspection shows
      initialization follows module creation order (the importer is reserved before recursive
      dependency loading), not a dependency-first order; TLS callback invocation is absent, and
      forwarded-export failures are not surfaced. Verify these paths with a representative DLL
      before expanding support.
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
      texture, vertex, index, structured, raw, typed, and constant-buffer `CopyResource`; texture
      `CopySubresourceRegion` and `UpdateSubresource`; DXBC clip/discard; 2D and 3D `GenerateMips`;
      fractional-LOD linear filtering; repeat/mirror/clamp sampler address pixels (BORDER and
      MIRROR_ONCE remain open) and an 8x anisotropic
      sampler request; instance step rates; render-to-texture; multiple render targets; cube, array,
      and 3D texture sampling; depth/stencil; cull, fill, scissor, and depth bias; 4x MSAA resolves
      for RGBA8, BGRA8, and RGBA16_FLOAT; and D3D11.1 constant-buffer ranges. Staging `Map(READ)`
      verifies RGBA8 back-buffer pixels, copied buffer values, two-row R32_FLOAT texels and
      R32_UINT/SINT words,
      R32G32_FLOAT two-channel texels with row pitches, R32G32_UINT/SINT packed channel words,
      RGBA32_FLOAT four-channel rows and RGBA32_UINT/SINT packed channel words with 32-byte rows,
      R16 and RG16 UNORM/SNORM/UINT/SINT words, RG16_FLOAT rows, and RGBA16_UNORM/SNORM/UINT/SINT
      words, plus packed RGBA16_FLOAT and R16_FLOAT values,
      raw BGRA8_UNORM bytes; R10G10B10A2_UNORM/UINT and R11G11B10_FLOAT packed words,
      encoded RGBA8_SRGB and BGRA8_SRGB bytes; RGBA8_UNORM and R8/RG8/RGBA8 SNORM/UINT/SINT plus
      A8_UNORM raw bytes; R8G8_UNORM two-channel texels; odd-width R8_UNORM row packing plus
      mip/array subresource selection; depth-only D16_UNORM and D32_FLOAT values; D32_FLOAT_S8X24
      depth/stencil/padding bytes when the optional WebGPU format is available; and Texture3D
      R32_FLOAT row/depth
      pitches. D24_UNORM_S8 depth and legacy packed 16-bit staging formats remain open; WebGPU's
      depth24plus representation is implementation-defined ([format rules](https://www.w3.org/TR/webgpu/#depth-stencil-formats)).
      Comparison samplers pixel-test all eight
      comparison functions against depth values on both sides of the reference; depth-state
      comparisons pixel-test all eight functions at equality. Stencil comparisons cover all eight
      functions with stored values below, equal to, and above the reference. Stencil pass operations
      cover keep, zero, replace, both saturating operations, invert, and both wrapping operations,
      including 8-bit boundaries. D24_UNORM_S8 and legacy packed 16-bit staging formats, other
      subresource layouts, other format/resolve
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
      UAV formats outside that advertised set, geometry/hull/domain shaders, general staging read-back, deferred
      contexts/command lists, `SV_` system values beyond the basics, tiled resources, and honest
      feature-level reporting remain open.
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
      at equality are also checked. PREMODULATE, bump-environment mapping, multi-stage texture
      cascades, programmable shaders, and broad device/state coverage remain open. D3D12,
      DXGI flip-model/fullscreen/resize/vsync
      semantics, OpenGL (WGL), and Vulkan remain open. Prior art (WineBrowser: D3D9/12→WebGPU, MIT)
      is documented in `docs/EVALUATION.md` — review licence/architecture before borrowing.
- [~] **Frame pacing**: after each `Stop::Frame`, the worker advances the virtual guest clock by the
      measured time spent waiting for browser `requestAnimationFrame`, converting elapsed 100 ns
      units to internal 10 ns ticks. A process regression and the selected target's movement replay
      verify this scale against its 10 MHz `QueryPerformanceCounter`. `Present` interval semantics,
      scheduling under JIT and offscreen canvas resize after transfer remain open.

## 4. CPU correctness
- [~] **AVX/BMI instruction coverage**: selected VEX forms now cover target-used VMOVUPS/APS,
      VMOVDQA/DQU, VMOVSS/SD, VPXOR/VXORPS, VADDSS/VDIVSS, VCOMISS/VUCOMISS, VROUNDSS,
      VCVTSI2SS/VCVTTSS2SI, VEXTRACTF128, VZEROUPPER, ANDN, SHLX, SHRX, BZHI, VPBROADCASTD,
      VPADDD, VMOVDQU, VMOVQ, VPEXTRD/Q, VCMPEQSS, VBLENDVPS, VMOVD, and BLSI. These are exact
      target-grounded forms, not broad AVX/AVX2/BMI2 support; remaining AVX/AVX2/BMI/FMA and
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
- [x] Confirm GitHub Actions is green on commits after `bd6f0bd` (incl. `631ba66`).
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
      import blockers in `docs/TARGETS.md`. Both tutorials now have D3D11 first-stop AnyWASM traces
      and reach virtual device discovery/creation, then stop at the unresolved MSVCP140
      `basic_ios<char>` constructor; neither reaches shaders or `Present`. Asteroids remains
      static-analysis only and needs a separate Intel-origin code/assets redistribution review.
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
      scoped to its seeded build and aim direction; see `docs/TARGETS.md`. The
      normal worker slice still stops in a synchronous callback that cannot resume. One pre-correction
      controlled seed-1 maze traversal reached the teleport; rerun it with the corrected frame clock.
      Broad gameplay and end-to-end game acceptance remain open; see `docs/TARGETS.md` for exact
      evidence and limits.
- [~] Packaging flow: the browser now shows the first reached unimplemented import in an accessible
      alert with the exact runtime message and `anywasm analyze` guidance; verified with the
      `unimpl.exe` fixture. It also shows phase-based load progress; browser coverage holds the
      runtime fetch and confirms the indicator clears once an input fixture reaches its message
      loop. Single-file/static-site export remains open. These fixtures do not satisfy the
      real-target acceptance gate above.
- [~] Browser support matrix: Chromium page/worker/input and WebGPU readback behavior, Firefox
      hello plus version-specific graphics availability, and WebKit hello plus its OffscreenCanvas
      limitation are recorded in `docs/BROWSER_SUPPORT.md`. Windows Firefox 141.0 exits the triangle
      fixture after three frames but reports a missing-bind-group validation error and renders black;
      Firefox 140.0.2 lacks WebGPU. The D3D11 triangle canvas blit is screenshot-checked only in
      Chromium. Large-game memory (4 GB / `memory64`)
      and asset streaming remain unverified. The
      `wasm32-unknown-unknown` browser path buffers the full bundle, copies it into WASM memory, then
      duplicates each file into the VFS; this source audit is not a peak-memory measurement.
- [~] Licensing review of vendored vkd3d-shader (LGPL) and Naga: technical inventory and pre-ship
      obligations are in `THIRD_PARTY.md`; legal review of the packaged combined work is still required.
- [x] Legal/scope statement: user supplies their own legally obtained binaries.
