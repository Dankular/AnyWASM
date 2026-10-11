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
node test/browser/cross-engine.mjs          # Firefox and WebKit core/graphics availability checks
bash fixtures/build.sh; node fixtures/gen_d3d_shaders.mjs   # rebuild fixtures / shader headers
node fixtures/build_bench.mjs      # rebuild the call, memory, and SSE benchmark PE fixtures (Rust Windows target)
```
Chromium path: highest `/opt/pw-browsers/chromium-NNNN/chrome-linux/chrome` (override with `CHROMIUM`).
The browser suite uses `?test=1` for direct back-buffer readback, and separately verifies the
visible 640x480 triangle canvas by screenshot without test mode. Other browser engines remain
partially exercised by `test/browser/cross-engine.mjs`; override browser paths with `FIREFOX` and
`WEBKIT`, or select one engine with `BROWSER=Firefox` / `BROWSER=WebKit`.

## Verified state (see docs/STATUS.md for detail)
CPU core differential-tested vs host (2,730 corpus entries); real mingw/msvcrt programs match native
output; D3D11 triangle (216 px, worst error 0/255) and `d3d11_textured` (texels, D32 depth ordering,
indexed draws) pixel-verified in headless Chromium; CI green through `bd6f0bd` (check later commits).

---

# Outstanding tasks

Status key: **[ ]** not started · **[~]** partial. Ordered by value toward "package a real game".

## 1. Block-translating JIT to WASM  (highest priority; performance)
Why: the cached-block executor's earlier baseline measured 32.2 M guest instr/s on `compute.exe` and
26.1–38.5 M on the call-heavy, memory-heavy and SSE-float fixtures (`test/bench.mjs`, release WASM
under Node). A later six-round gate sweep did not show a reliable improvement from dynamic emission,
so it is disabled by default. I have **not** measured what a real game needs; the 10–50x figure in
STATUS.md is an estimate. Measure first (e.g. instructions per frame of a representative program)
before fixing a target.

Design (partial implementation; the verified dynamic-emission scope is a leading run of 32-bit or
64-bit GPR-immediate and GPR-to-GPR MOV instructions at a block entry):
- [x] **Baseline**: add a benchmark set beyond `compute.exe` (call-heavy, memory-heavy, SSE float
      loops) and record instr/s in CI (report-only; no throughput threshold yet).
- [x] **Block discovery**: decode and cache basic blocks keyed by guest RIP. `code_epoch` invalidates
      cached blocks on executable-memory writes and protection changes.
- [~] **WASM emission (MVP prototype, opt-in)**: Rust can emit a separate module for the supported
      MOV prefix; JS compiles
      and instantiates it through the `anywasm.run_emitted_block` import. Generated modules import
      the runtime's exported memory. The JS side caches up to 256 modules and checks bytes when a
      process-local module ID is reused. `{ jitMinPrefix: 1 }` explicitly opts in; the default
      threshold is 65, above the 64-instruction block limit, so short blocks avoid host compile and
      execute overhead. No throughput win is established; module batching and performance remain open.
- [~] **State model (MVP)**: generated code receives a 160-byte `repr(C)` snapshot containing GPRs,
      RIP, RFLAGS, retired count, and TSC. It writes GPRs, RIP, count, and TSC; RFLAGS pass through
      unchanged. Rust commits the snapshot only when the returned retired count matches the budget
      and emitted prefix. Register-local allocation and lazy flags remain open.
- [ ] **Memory access**: fast path through the existing 3-level page table/TLB, with an inline TLB
      probe in generated code and a slow-path call for misses/faults.
- [ ] **Exits**: direct block chaining for known targets; indirect jumps via hash lookup;
      `call_guest`, HLE thunk addresses (region `0x7ffe_0000_0000`), `Stop::*`, `skip_ret`,
      `yield_request`, step limit/icount accounting must all keep working.
- [x] **Fallback**: only 32-bit and 64-bit GPR-immediate or GPR-to-GPR MOVs at a block's beginning
      are emitted. Partial-width and memory MOVs and all remaining instructions use the interpreter.
      32-bit register moves zero-extend; self-moves still advance RIP/count/TSC. Compile/trap/
      invalid-count failures retry from the untouched live CPU snapshot.
- [~] **Correctness checks**: Rust emitter tests cover prefix limits, 32-bit zero-extension,
      64-bit copies, self-move retirement, and that prefixes below the configured gate are skipped.
      The WASM smoke explicitly forces threshold 1 and confirms generated
      register-register MOV execution, checks `compute.exe` output and `run(1)` retirement limits,
      and compares the same bundle with the default cached-only path and `{ jit: false }` for status,
      exit code, stdout/stderr, and retired count. A forced host compile failure also falls back
      safely. The Chrome suite also explicitly runs the emitted path and compares its compute output
      and retired count with cached-only execution. Final registers/memory and the full
      JIT/interpreter/host differential remain unverified.
- [ ] SSE/x87 in the JIT: SSE via WASM SIMD where semantics match (verify per instruction —
      MXCSR rounding/denormal behaviour and NaN payloads differ); x87 may stay interpreter-only at first.
- [~] **Browser check**: the rebuilt runtime passes the headless Chromium worker, input, and WebGPU
      suite; page-realm and worker checks force the opt-in emitter and compare results with the
      cached-only path. The page-realm check grows WASM memory by one page after emitted execution
      begins and verifies the cached generated module resumes without fallback. Firefox 141.0 runs
      the hello and D3D11 triangle fixtures through its page and worker; Playwright WebKit 18.2 runs
      hello and reports graphics unavailable when OffscreenCanvas transfer is missing. CSP behavior,
      module-cache pressure, and full graphics validation on other engines remain open.

## 2. Windows runtime coverage
- [~] **Threads**: finite `CreateThread` callbacks run inline on a private guest stack with
      separate TLS/FLS maps, thread ID, TEB stack bounds, last-error state, and exit status.
      This is not a scheduler: `_beginthreadex` remains unimplemented, blocking/infinite
      `SleepConditionVariableSRW/CS` stops as unimplemented, and critical sections, events,
      semaphores, `WaitForMultipleObjects`, and `Sleep` still need real synchronization semantics.
      Long-running callbacks stop at an instruction budget. Choose a scheduler model before adding
      resumable or concurrent guest threads.
- [ ] **SEH / C++ exceptions**: `.pdata` unwind tables, `RtlUnwindEx`/`RtlLookupFunctionEntry`,
      `__C_specific_handler`, `RaiseException`, vectored handlers; guest faults → exceptions.
- [~] **More of kernel32/ntdll/user32**: target reports confirm `InitializeSListHead` is imported
      and implemented in both tutorials; `GetCapture`, `IsWindowUnicode`, `GetDlgItem`, and
      `EnumDisplaySettingsW` cover reported USER32 imports. The tutorials now report 12 missing
      USER32 symbols each. The target binaries also import `SleepConditionVariableSRW`; the HLE
      stops explicitly on its infinite wait because a guest-thread scheduler and wait model are not
      implemented.
- [~] **GDI / gdi32**: virtual display DC creation and basic display capabilities are available;
      drawing, common controls, message boxes, and clipboard remain open as needed by real programs.
- [ ] **Audio**: winmm waveOut, DirectSound, XAudio2, WASAPI → WebAudio (AudioWorklet).
- [~] **Input devices**: `test/browser/run.mjs` verifies Playwright `WM_KEYDOWN` and
      `WM_LBUTTONDOWN` messages reach the guest; add XInput (Gamepad API), DirectInput, raw input,
      cursor capture/pointer lock.
- [ ] **Registry, COM beyond D3D/DXGI, shell/known-folders, networking (winsock → WebSocket/
      fetch relay)**, timers/QueryPerformanceCounter accuracy, file dialogs, persistent VFS
      (IndexedDB/OPFS) — verify what is already implemented before starting each.
- [ ] **32-bit PE (WoW64-style)**: needs a 32-bit decoder/execution mode, 32-bit TEB/PEB and
      32-bit HLE ABI. Large; decide after the JIT (many old games are 32-bit).
- [ ] **DLL loading**: bundled DLLs are supported by the loader; verify behaviour with real
      third-party DLLs (init order, `DllMain`, TLS callbacks, forwarded exports).
- [ ] **Packer/analyzer**: `anywasm pack` UX (assets, args, working dir), `analyze` JSON schema
      stability, a "will this run?" report that lists blockers before packaging.

## 3. Graphics
- [~] **D3D11 verification gaps** (implemented in the translator; constant blend-factor blending,
      one 2x2 texture-to-texture `CopyResource` case, and one same-size vertex-buffer
      `CopyResource` case are pixel-verified in `test/browser/run.mjs` via `d3d11_textured`):
      other buffer `CopyResource` cases, other blend modes,
      alpha test/discard, mips and trilinear/anisotropic filtering, instancing,
      `CopySubresourceRegion`/`UpdateSubresource`, render-to-texture, multiple render targets,
      cube/array/3D textures, depth-stencil ops/stencil, rasterizer state
      (cull/fill/scissor/bias), `GenerateMips`, constant-buffer packing edge cases.
- [ ] **D3D11 unimplemented**: compute shaders + UAVs/structured/raw buffers, geometry/hull/domain
      shaders, MSAA (forced to 1 sample), staging read-back of GPU-written data, queries,
      deferred contexts/command lists, `SV_` system values beyond the basics, tiled resources,
      feature levels reported honestly.
- [ ] **Shader toolchain**: broader DXBC coverage in vkd3d-shader → SPIR-V → Naga → WGSL; collect
      failing real-world shaders as test cases; consider shader/pipeline caching (IndexedDB) and
      async pipeline creation to avoid stalls.
- [~] **Canvas blit pixels**: the visible D3D11 triangle's clear colour and interpolated centre
      pixel are checked from a 640x480 Chromium screenshot without test mode. Other render paths,
      canvas sizes, and browser engines still need coverage.
- [ ] **Other APIs**: D3D9 (very common in older games), D3D12, DXGI flip-model/fullscreen/
      resize/vsync semantics, OpenGL (WGL), Vulkan. Prior art (WineBrowser: D3D9/12→WebGPU, MIT)
      is documented in `docs/EVALUATION.md` — review licence/architecture before borrowing.
- [ ] **Frame pacing**: `Present` interval vs rAF, `Stop::Frame` scheduling under JIT, offscreen
      canvas resize, HiDPI.

## 4. CPU correctness
- [~] **AVX/BMI instruction coverage**: selected VEX forms now cover target-used VMOVUPS/APS,
      VMOVDQA/DQU, VMOVSS/SD, VPXOR/VXORPS, VADDSS/VDIVSS, VCOMISS/VUCOMISS, VROUNDSS,
      VCVTSI2SS/VCVTTSS2SI, VZEROUPPER, ANDN, and SHLX. This is a targeted subset, not AVX2/BMI2
      support; the remaining AVX/AVX2/BMI/FMA and AES/PCLMUL/MMX instructions are still open.
- [ ] **x87 precision**: currently f64, not 80-bit; implement soft-float 80-bit if a target needs it.
- [ ] `rcpps`/`rsqrtps` return exact results (excluded from the diff corpus); match hardware
      approximation or document per-vendor differences.
- [~] Extend the diff harness to control flow (jcc/call/ret/loop/string ops with REP), privileged-ish
      instructions used by user code (`rdtsc`, `cpuid`, `xgetbv`, `syscall` stubs), and multi-
      instruction sequences. Added LOOP/Jcc, direct CALL/RET, and REP string cases; portable REP
      checks cover zero count, DF direction, operand widths 8/16/32/64, a 32-bit address-size
      override, and early termination. Linux-only host differential cases cover byte-sized
      operations and are present but unrun here. Undefined
      flags/vendor-specific behaviour must be excluded and documented (e.g. 16-bit shld/shrd count>16).
- [ ] Anti-debug/anti-cheat/DRM: out of scope unless a target requires it; flag as unsupported.

## 5. Tooling, CI, repo hygiene
- [x] **Copy `ci/web.yml` to `.github/workflows/`** (mirrored as `web.yml`; requires `workflow` scope).
- [x] Confirm GitHub Actions is green on commits after `bd6f0bd` (incl. `631ba66`).
- [x] Removed the tracked two-line root `out.txt` added in `800f4e1` and deleted in `1c8246c`.
      `fixtures/crt_test.c` separately writes a guest `out.txt` during its CRT test.
- [x] Silence/resolve compiler warnings (e.g. unused `ERROR_ACCESS_DENIED` in `kernel32.rs`).
- [x] Add a Rust-side command-stream test for `d3d11_textured` (`tests/d3d11_stream.rs` covers the
      triangle only) and add the textured exe to `test/wasm_smoke.mjs`.
- [x] Performance dashboard: record bench numbers per commit in CI (non-gating at first).
- [~] Task list housekeeping: GitHub's all-state issue list is empty, so historical #5/#11/#6
      references cannot be mapped to issue records. The known task notes mark #5/#11 effectively
      done for triangle/textured slices and #6 (docs) ongoing; broader status cannot be recovered.

## 6. Productisation (after a real target runs)
- [~] Analyze three Win64 Diligent samples (HelloTriangle, Texturing, Asteroids) and record their
      import blockers in `docs/TARGETS.md`. These are preliminary targets: the tutorial renderer
      choice was not launched, the samples have not run through AnyWASM, and Asteroids needs a
      separate Intel-origin code/assets redistribution review. The Apache-2.0 GLFW Demo maze is now
      the selected game candidate. A separate D3D11-only `/MT` build succeeded; the current
      analyzer reports 96 missing imports across its EXE and three DLLs, down from 349. Its
      `MultiByteToWideChar` and `LCMapStringEx` size-query faults are fixed and covered. The bounded
      AnyWASM run reaches `SleepConditionVariableSRW` after finite thread callbacks, then stops
      because the scheduler cannot resume a blocked guest thread. The end-to-end acceptance gate
      remains open; see `docs/TARGETS.md` for exact reports and next steps.
- [~] Packaging flow: the browser now shows the first reached unimplemented import in an accessible
      alert with the exact runtime message and `anywasm analyze` guidance; verified with the
      `unimpl.exe` fixture. It also shows phase-based load progress; browser coverage holds the
      runtime fetch and confirms the indicator clears once an input fixture reaches its message
      loop. Single-file/static-site export remains open. These fixtures do not satisfy the
      real-target acceptance gate above.
- [~] Browser support matrix: Chromium page/worker/input and WebGPU readback behavior, Firefox
      hello/triangle execution, and WebKit hello plus its OffscreenCanvas limitation are recorded in
      `docs/BROWSER_SUPPORT.md`. The D3D11 triangle canvas blit is screenshot-checked only in
      Chromium. Large-game memory (4 GB / `memory64`) and asset streaming remain unverified.
- [~] Licensing review of vendored vkd3d-shader (LGPL) and Naga: technical inventory and pre-ship
      obligations are in `THIRD_PARTY.md`; legal review of the packaged combined work is still required.
- [x] Legal/scope statement: user supplies their own legally obtained binaries.
