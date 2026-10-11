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
| `test/` | `wasm_smoke.mjs`, `bench.mjs`, `shaders.mjs`, `browser/run.mjs` (Playwright, headless Chromium) |
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
bash fixtures/build.sh; node fixtures/gen_d3d_shaders.mjs   # rebuild fixtures / shader headers
node fixtures/build_bench.mjs      # rebuild the call, memory, and SSE benchmark PE fixtures (Rust Windows target)
```
Chromium path: highest `/opt/pw-browsers/chromium-NNNN/chrome-linux/chrome` (override with `CHROMIUM`).
Test-environment caveat: in this headless Chromium `getCurrentTexture()` breaks later `mapAsync` and
screenshots don't capture WebGPU canvases, so tests use `?test=1` (skips the canvas blit).

## Verified state (see docs/STATUS.md for detail)
CPU core differential-tested vs host (2,730 corpus entries); real mingw/msvcrt programs match native
output; D3D11 triangle (216 px, worst error 0/255) and `d3d11_textured` (texels, D32 depth ordering,
indexed draws) pixel-verified in headless Chromium; CI green through `bd6f0bd` (check later commits).

---

# Outstanding tasks

Status key: **[ ]** not started · **[~]** partial. Ordered by value toward "package a real game".

## 1. Block-translating JIT to WASM  (highest priority; performance)
Why: the interpreter measures ~19 M guest instr/s (confirmed, `test/bench.mjs`, WASM under Node).
I have **not** measured what a real game needs; the 10–50x figure in STATUS.md is an estimate.
Measure first (e.g. instructions per frame of a representative program) before fixing a target.

Design (proposal, **unverified** — nothing below is implemented):
- [x] **Baseline**: add a benchmark set beyond `compute.exe` (call-heavy, memory-heavy, SSE float
      loops) and record instr/s; keep as a regression gate.
- [ ] **Block discovery**: translate basic blocks keyed by guest RIP, using the existing decoder.
      Reuse `Memory::exec_page_count` / `code_epoch` for self-modifying-code invalidation.
- [ ] **WASM emission**: emit a WASM module per block (or per trace) from Rust, instantiate via a
      host import (`WebAssembly.Module`/`Instance` from JS; the Rust core runs *inside* WASM, so
      compiled blocks must be called through an imported trampoline / `call_indirect` table).
      Need to decide and measure: per-block module instantiate cost vs batching blocks.
- [ ] **State model**: guest registers in a linear-memory struct (or WASM locals within a block,
      spilled at block exit); lazy flags (compute flags only when consumed) — the interpreter's
      current flag handling must be read before choosing.
- [ ] **Memory access**: fast path through the existing 3-level page table/TLB, with an inline TLB
      probe in generated code and a slow-path call for misses/faults.
- [ ] **Exits**: direct block chaining for known targets; indirect jumps via hash lookup;
      `call_guest`, HLE thunk addresses (region `0x7ffe_0000_0000`), `Stop::*`, `skip_ret`,
      `yield_request`, step limit/icount accounting must all keep working.
- [ ] **Fallback**: any instruction not yet translated falls back to the interpreter for that block
      (so the JIT can grow incrementally; never silently mis-translate).
- [ ] **Correctness gate**: extend `tests/diff.rs` to run each corpus entry through the JIT as well
      as the interpreter and the host CPU; add a block-level differential (run whole fixtures with
      JIT on/off and compare registers/memory/output).
- [ ] SSE/x87 in the JIT: SSE via WASM SIMD where semantics match (verify per instruction —
      MXCSR rounding/denormal behaviour and NaN payloads differ); x87 may stay interpreter-only at first.
- [ ] Browser constraints to check: JIT in the worker, CSP, `WebAssembly.compile` of many small
      modules, memory growth interaction with `Memory.buffer` detachment.

## 2. Windows runtime coverage
- [ ] **Threads**: `CreateThread`, `_beginthreadex`, TLS per thread, critical sections/events/
      semaphores/`WaitForMultipleObjects`, `Sleep`. Needs a design decision (cooperative green
      threads in one worker vs SharedArrayBuffer + Web Workers; SAB is available in the test
      browser per the capabilities line).
- [ ] **SEH / C++ exceptions**: `.pdata` unwind tables, `RtlUnwindEx`/`RtlLookupFunctionEntry`,
      `__C_specific_handler`, `RaiseException`, vectored handlers; guest faults → exceptions.
- [ ] **More of kernel32/ntdll/user32**: driven by `anywasm analyze` on real target programs, not
      guessed. Use the per-import report to prioritise.
- [ ] **GDI / gdi32**, common controls, message boxes, clipboard (as needed by real programs).
- [ ] **Audio**: winmm waveOut, DirectSound, XAudio2, WASAPI → WebAudio (AudioWorklet).
- [ ] **Input devices**: keyboard/mouse → window messages is implemented but has **no automated
      interaction test**; add one (Playwright key/mouse events → guest observes). Add
      XInput (Gamepad API), DirectInput, raw input, cursor capture/pointer lock.
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
- [ ] **D3D11 verification gaps** (implemented in the translator but *not* pixel-verified):
      blending, alpha test/discard, mips + trilinear/anisotropic filtering, instancing,
      `CopyResource`/`CopySubresourceRegion`/`UpdateSubresource`, render-to-texture, multiple
      render targets, cube/array/3D textures, depth-stencil ops/stencil, rasterizer state
      (cull/fill/scissor/bias), `GenerateMips`, constant-buffer packing edge cases.
- [ ] **D3D11 unimplemented**: compute shaders + UAVs/structured/raw buffers, geometry/hull/domain
      shaders, MSAA (forced to 1 sample), staging read-back of GPU-written data, queries,
      deferred contexts/command lists, `SV_` system values beyond the basics, tiled resources,
      feature levels reported honestly.
- [ ] **Shader toolchain**: broader DXBC coverage in vkd3d-shader → SPIR-V → Naga → WGSL; collect
      failing real-world shaders as test cases; consider shader/pipeline caching (IndexedDB) and
      async pipeline creation to avoid stalls.
- [ ] **Canvas blit pixels are unverified** in this environment (see caveat above). Verify on a
      configuration where screenshots capture WebGPU, or via a different readback path.
- [ ] **Other APIs**: D3D9 (very common in older games), D3D12, DXGI flip-model/fullscreen/
      resize/vsync semantics, OpenGL (WGL), Vulkan. Prior art (WineBrowser: D3D9/12→WebGPU, MIT)
      is documented in `docs/EVALUATION.md` — review licence/architecture before borrowing.
- [ ] **Frame pacing**: `Present` interval vs rAF, `Stop::Frame` scheduling under JIT, offscreen
      canvas resize, HiDPI.

## 4. CPU correctness
- [ ] **AVX/AVX2/BMI/FMA (VEX) and AES/PCLMUL/MMX**: not decoded; CPUID currently hides them.
      Many modern binaries require AVX2 or BMI2 — decide whether to implement or keep hiding.
- [ ] **x87 precision**: currently f64, not 80-bit; implement soft-float 80-bit if a target needs it.
- [ ] `rcpps`/`rsqrtps` return exact results (excluded from the diff corpus); match hardware
      approximation or document per-vendor differences.
- [ ] Extend the diff harness to control flow (jcc/call/ret/loop/string ops with REP), privileged-ish
      instructions used by user code (`rdtsc`, `cpuid`, `xgetbv`, `syscall` stubs), and multi-
      instruction sequences. Remember: undefined flags/vendor-specific behaviour must be excluded
      explicitly and documented (precedent: 16-bit shld/shrd count>16).
- [ ] Anti-debug/anti-cheat/DRM: out of scope unless a target requires it; flag as unsupported.

## 5. Tooling, CI, repo hygiene
- [x] **Copy `ci/web.yml` to `.github/workflows/`** (mirrored as `web.yml`; requires `workflow` scope).
- [x] Confirm GitHub Actions is green on commits after `bd6f0bd` (incl. `631ba66`).
- [x] `out.txt` at the repo root was confirmed as an unreferenced two-line sample added in `800f4e1`; remove it.
- [x] Silence/resolve compiler warnings (e.g. unused `ERROR_ACCESS_DENIED` in `kernel32.rs`).
- [x] Add a Rust-side command-stream test for `d3d11_textured` (`tests/d3d11_stream.rs` covers the
      triangle only) and add the textured exe to `test/wasm_smoke.mjs`.
- [x] Performance dashboard: record bench numbers per commit in CI (non-gating at first).
- [~] Task list housekeeping: the repository has no GitHub issues to reconcile the old #5/#11/#6
      references against. The triangle/textured slices are covered; docs still evolve with verified work.

## 6. Productisation (after a real target runs)
- [~] Analyze three Win64 Diligent samples (HelloTriangle, Texturing, Asteroids) and record their
      import blockers in `docs/TARGETS.md`. These are preliminary targets: the tutorial renderer
      choice was not launched, the samples have not run through AnyWASM, and Asteroids needs a
      separate Intel-origin code/assets redistribution review. Still choose and run a redistributable
      game as an end-to-end acceptance target.
- [ ] Packaging flow: single-file/static-site export of exe + assets + runtime; loading UI,
      progress, error reporting that surfaces the exact missing `dll!Symbol`.
- [ ] Browser support matrix (WebGPU features/limits, Safari/Firefox), memory limits of large
      games (4 GB WASM memory / `memory64`), asset streaming.
- [~] Licensing review of vendored vkd3d-shader (LGPL) and Naga: technical inventory and pre-ship
      obligations are in `THIRD_PARTY.md`; legal review of the packaged combined work is still required.
- [x] Legal/scope statement: user supplies their own legally obtained binaries.
