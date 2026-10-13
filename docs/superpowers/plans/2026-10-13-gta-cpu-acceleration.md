# GTA CPU Acceleration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable useful JIT and concurrent guest CPU execution, then measure GTA against the 10x loading and CPU-throughput target.

**Architecture:** Keep the process coordinator authoritative and move only bounded guest CPU slices to emitted WASM or isolated shared-memory CPU workers. Validate JIT failure diagnostics against the target, extend emission for the profiled GTA parser path, and prove concurrent guest-thread overlap. Finish with paired GTA browser measurements and retain any unachieved 10x criterion as open work.

**Tech Stack:** Rust x86 decoder/emitter and runtime, Rust `no_std` shared CPU worker, JavaScript `WebAssembly` host/JIT imports, SharedArrayBuffer, Playwright Chromium, local GTA San Andreas package.

**Spec:** `docs/superpowers/specs/2026-10-13-gta-cpu-acceleration-design.md`

## Global Constraints

- Preserve interpreter fallback and exact guest instruction accounting.
- The coordinator remains authoritative for HLE, process state, scheduler decisions, exceptions, and GPU work.
- Shared guest memory uses atomic access; never introduce Rust data races through a shared slice.
- Measure the same GTA executable, data bundle, browser, and input sequence for baseline and optimized runs.
- Do not mark the 10x target met without gameplay-load and CPU-limited gameplay measurements.

## Review Focus

- A rejected or trapping emitted module must return to the interpreter with unchanged state and expose its failure reason.
- x86-32 register width, flags, segmented addressing, and page-crossing accesses must match interpreted execution.
- A guest thread blocking or entering HLE while other workers execute must not lose scheduler or process state.
- Workers must not overlap contexts that share unsynchronized guest state unless the supported x86 atomic path protects it.
- GTA progress must distinguish asset waiting from CPU parsing and count gameplay frames rather than splash/loading presents.

---

### Task 1: Surface JIT failure causes

**Files:**
- Modify: `web/anywasm.js`
- Test: `test/wasm_smoke.mjs`

**Interfaces:**
- Consumes: `AnyWasm.jitStats` and the existing injected `WebAssembly.Module` compile-failure fixture.
- Produces: `jitStats.lastFailure`, containing the latest emitted-module exception message or `null` before any failure.

- [x] The forced module-compile and generated-module-trap regressions assert bounded `lastFailure` text and verify interpreter parity.
- [x] The host records bounded exception text for module compilation and generated-code traps.
- [x] Run `node test/wasm_smoke.mjs`; both forced-failure cases pass with output, retired count, CPU state, and memory matching the interpreter.
- [x] Run the GTA harness with `GTA_JIT_MIN_PREFIX=1`. No compile exception occurred (`lastFailure` stayed null); after 1,804,686 guest instructions the probe had 268,068 compilations, 266,175 executions, 414,849 emitted instructions, 12,630 fallbacks, and no visible game window within 60 seconds. Task 2 should target unsupported parser instructions, not a compile error.

### Task 2: Extend emission for the GTA x86-32 parser path

**Files:**
- Modify: `crates/anywasm-cpu/src/jit.rs`
- Test: `crates/anywasm-cpu` JIT unit tests, then a targeted WASM differential fixture

**Interfaces:**
- Consumes: The decoded scanner pair at `gta_sa.exe+0x82919c` and the existing `emit_mov_block_mode` / differential fixture APIs.
- Produces: Correct emitted blocks for target-observed 32-bit scanner instructions, with complete CPU and mapped-memory equivalence.

- [x] Add a focused emitter regression for `MOVZX EBX, byte ptr [ESI]` followed by `MOVZX EAX, BL`; verify guarded byte-load and zero-extension code generation.
- [x] Run the focused test red before the implementation and green after it; all 45 `anywasm-cpu` library tests pass.
- [x] Add a WASM differential fixture for the decoded instruction pair; assert interpreter, cached, and emitted execution have equal retired count, full CPU state, and mapped memory. `node test/wasm_jit_x86_movzx.mjs` passes with 10 retired instructions, 2 emitted executions, and matching 776-byte CPU / 2,204,385-byte mapped-memory snapshots.
- [ ] Run a forced-emission GTA replay with the new pair and confirm nonzero target execution counters without disrupting the normal loading path.

### Task 3: Accelerate the measured GTA parser path

**Files:**
- Modify: `crates/anywasm-cpu/src/jit.rs`, `crates/anywasm-cpu/src/exec.rs`
- Test: new targeted 32-bit JIT fixture plus existing CPU/WASM differential suites

**Interfaces:**
- Consumes: Task 2's reliable 32-bit emitted path and `docs/TARGETS.md` parser trace at `gta_sa.exe+0x4290d4`.
- Produces: Emitted loops covering the frequently executed, measured parser instruction sequence without changing guest-visible results.

- [ ] Add the exact hot instruction sequence found in a current GTA trace to a fixture and verify it fails the emitted-throughput acceptance threshold while preserving interpreter parity.
- [ ] Add the next profiled x86-32 operations to the emitter, one tested instruction family at a time, until the hot loop no longer falls back on its main path.
- [ ] Compare cached-only and emitted guest throughput on the fixture, requiring exact status, output, retired count, CPU state, and mapped memory.
- [ ] Benchmark GTA at fixed post-New-Game checkpoints; record emitted instructions per second, compilation count, execution count, fallbacks, presents, and pending ranges.
- [ ] Keep a compatible hotness/prefix threshold as the default only when GTA and representative fixture throughput improve over cached-only execution.

Progress: x86-32 emission now includes the two scaled `LEA` instructions at `gta_sa.exe+0x8291b0`.
The PE32 differential fixture covers the measured `MOVZX` pair and `LEA` pair; interpreter, cached,
and emitted modes agree on exit status, 15 retired instructions, full CPU state, and mapped memory.
This is instruction-family coverage only; the parser's helper-call path and GTA throughput benchmark
remain open.

### Task 4: Dispatch independently runnable GTA guest threads concurrently

**Files:**
- Modify: `web/worker.js`, `web/shared-cpu-pool.js`, `web/shared-cpu-worker.js`, `crates/anywasm-rt/src/process.rs`, shared CPU ABI as required.
- Test: shared worker unit/integration fixtures and browser worker overlap fixture.

**Interfaces:**
- Consumes: existing shared CPU snapshot ABI, CPU worker entry point, completion states, coordinator turn and runnable-thread APIs.
- Produces: concurrent bounded slices for up to the configured CPU-worker count of distinct runnable guest thread contexts, with completion import and process-wide effects serialized by the coordinator.

- [x] Add a failing two-context overlap fixture using an atomic rendezvous and shared-memory sentinel; prove both workers execute at once and preserve distinct register/stack state.
- [x] Add a failing coordinator test where one worker returns an HLE/fault/exit completion while another context is still running; assert no process-wide mutation occurs before the safe point.
- [x] Implement concurrent dispatch only for distinct runnable contexts and supported shared-memory instructions; retain serial fallback for one runnable context, unsupported atomics, or worker initialization failure.
- [x] Run shared worker CPU tests and browser overlap test; require exact retired-count accounting and no lost or duplicated context completions.
- [ ] Run the GTA replay with workers enabled; record actual overlap, active worker count, loader time, and CPU throughput against the cooperative run.

Progress: On 2026-10-13, the shared-worker Chromium fixture observed two active CPU slices while both guest threads completed an atomic rendezvous and preserved separate stack sentinels. This exposed a missing 32-byte Win64 home area in new-thread startup; x64 threads now begin at `stack_base - 0x28`, matching the main-thread ABI. The selected GLFW target still reports zero two-context batches, so GTA target overlap and throughput remain open.

### Task 5: GTA 10x acceptance benchmark

**Files:**
- Create: `test/browser/gta-performance.mjs`
- Modify: `docs/TARGETS.md`, `docs/STATUS.md`, `CLAUDE.md`

**Interfaces:**
- Consumes: the local game package, the same Playwright/Chrome setup as `target-gta-diag/gta-start-game.mjs`, JIT stats, worker mode, GPU present count, and `__workerDiag` progress.
- Produces: JSON captures for cooperative and optimized runs, including time to gameplay and 60-second CPU-limited gameplay throughput.

- [ ] Create a harness that sends menu inputs at observed milestones, samples progress without per-instruction instrumentation, and records the exact binary/package hashes.
- [ ] Confirm the harness detects the loading-artwork and first-gameplay transitions separately.
- [ ] Run at least three cooperative baseline and three optimized runs with fresh browser contexts; compute medians for gameplay load time and game CPU/frame throughput.
- [ ] Require both medians to meet the 10x target before marking the objective complete; otherwise record measured gains and continue optimization.
- [ ] Update runtime docs with actual mode activation, failure/fallback behavior, limitations, and measurements.
