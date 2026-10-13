# GTA CPU Acceleration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable useful JIT and concurrent guest CPU execution, then measure GTA against the 10x loading and CPU-throughput target.

**Architecture:** Keep the process coordinator authoritative and move only bounded guest CPU slices to emitted WASM or isolated shared-memory CPU workers. First expose and fix the current x86-32 JIT failure; then optimize the profiled GTA parser path and prove concurrent guest-thread overlap. Finish with paired GTA browser measurements and retain any unachieved 10x criterion as open work.

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

- [ ] Add an assertion to the existing forced compile-failure regression that `lastFailure` equals `forced emitted-module compile failure`.
- [ ] Run `node test/wasm_smoke.mjs`; confirm the new assertion fails because `lastFailure` is absent.
- [ ] Store bounded exception text in `jitStats.lastFailure` in the host JIT failure paths, including module compilation and generated-module traps.
- [ ] Re-run `node test/wasm_smoke.mjs`; confirm the forced failure is reported while output, retired count, CPU state, and memory still match the interpreter.
- [ ] Run the GTA harness with `GTA_JIT_MIN_PREFIX=1`; capture the actual compile failure reason and use it to select Task 2's regression.

### Task 2: Make GTA x86-32 hot blocks emit and execute correctly

**Files:**
- Modify: `crates/anywasm-cpu/src/jit.rs`, `crates/anywasm-cpu/src/exec.rs`
- Test: `crates/anywasm-cpu` JIT unit tests and `test/wasm_smoke.mjs`

**Interfaces:**
- Consumes: Task 1's failure reason and the existing `emit_mov_block_mode` / differential fixture APIs.
- Produces: Correct emitted blocks for the exact failing GTA-observed 32-bit instruction sequence, with complete CPU and mapped-memory equivalence.

- [ ] Add a failing differential fixture for the decoded instruction sequence identified by Task 1; assert interpreter, cached, and emitted execution have equal retired count, full CPU state, and mapped memory.
- [ ] Run the focused test and confirm it fails on the current emitted path.
- [ ] Fix only the failing emitter/host-trampoline behavior and retain fallback at any unsupported instruction or unsafe memory boundary.
- [ ] Run the focused Rust and WASM differential checks; require a successful compile and execution counter with no failure for the target sequence.
- [ ] Run the GTA harness forced-emission probe and verify nonzero successful compilation/execution counters and no repeated compile-failure count.

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

### Task 4: Dispatch independently runnable GTA guest threads concurrently

**Files:**
- Modify: `web/worker.js`, `web/shared-cpu-pool.js`, `web/shared-cpu-worker.js`, `crates/anywasm-rt/src/process.rs`, shared CPU ABI as required.
- Test: shared worker unit/integration fixtures and browser worker overlap fixture.

**Interfaces:**
- Consumes: existing shared CPU snapshot ABI, CPU worker entry point, completion states, coordinator turn and runnable-thread APIs.
- Produces: concurrent bounded slices for up to the configured CPU-worker count of distinct runnable guest thread contexts, with completion import and process-wide effects serialized by the coordinator.

- [ ] Add a failing two-context overlap fixture using an atomic rendezvous and shared-memory sentinel; prove both workers execute at once and preserve distinct register/stack state.
- [ ] Add a failing coordinator test where one worker returns an HLE/fault/exit completion while another context is still running; assert no process-wide mutation occurs before the safe point.
- [ ] Implement concurrent dispatch only for distinct runnable contexts and supported shared-memory instructions; retain serial fallback for one runnable context, unsupported atomics, or worker initialization failure.
- [ ] Run shared worker CPU tests and browser overlap test; require exact retired-count accounting and no lost or duplicated context completions.
- [ ] Run the GTA replay with workers enabled; record actual overlap, active worker count, loader time, and CPU throughput against the cooperative run.

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
