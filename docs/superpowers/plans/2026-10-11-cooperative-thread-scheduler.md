# Cooperative Thread Scheduler Implementation Plan

> **For agentic workers:** execute the steps in order; each produces a testable deliverable.

**Goal:** Resume blocked Win64 guest threads and implement the synchronization behavior needed by
the selected Diligent GLFW Demo wait path.

**Architecture:** Keep one host CPU and shared guest memory, with a deterministic ready queue of
full saved CPU contexts. HLE synchronization calls update thread state and request a switch at the
return boundary; timer deadlines use a process-wide virtual clock.

**Tech Stack:** Rust runtime/HLE, freestanding Win64 C fixtures, existing Node WASM smoke suite.

**Spec:** `docs/superpowers/specs/2026-10-11-cooperative-thread-scheduler.md`

## Global Constraints

- Keep changes on `claude/upbeat-franklin-c1vbxr`.
- Missing imports continue to stop with their exact `dll!symbol`.
- Never return success from a synchronization call whose state transition did not occur.
- Run the full workspace suite and the WASM smoke suite before claiming the scheduler milestone.
- The real-target trace remains bounded and does not imply renderer or gameplay acceptance.

## Review Focus

- A thread that blocks inside its first callback must resume at the instruction after the wait.
- Switching CPU snapshots must preserve SIMD, x87, flags, TEB fields, TLS/FLS, and last error.
- Timeouts must use process-wide monotonic virtual time across thread switches.
- Wait-all must not consume a partial set of objects when one remains unavailable.
- A guest thread that never enters HLE must still yield at the instruction quantum.

---

### Task 1: Per-thread execution context

**Files:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-rt/tests/run_exe.rs`

- [x] Add a `GuestThread` context holding complete `Cpu`, stack bounds, last error, TLS/FLS, and
  Ready/Running/Blocked/Exited state.
- [x] Add save/restore helpers that switch the active thread's TEB fields and TLS/FLS maps without
  changing shared guest memory or module state.
- [x] Add a guest regression that switches between two finite threads and proves full context and
  TLS isolation.

### Task 2: Ready queue and resumable thread callbacks

**Files:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-rt/src/hle/kernel32.rs`,
`crates/anywasm-rt/tests/run_exe.rs`, `fixtures/thread_test.c`, `fixtures/build.sh`

- [x] Replace synchronous `CreateThread` callback execution with a saved guest context and ready
  queue entry; preserve initial suspend and `ResumeThread` behavior.
- [x] Add an internal scheduler-yield signal handled by `Process::run` after the HLE thunk has
  returned to guest code.
- [x] Detect guest thread return and `ExitThread`, store its exit code, and wake thread-handle
  waiters.
- [x] Add an instruction quantum and process-wide retired-instruction accounting.
- [x] Verify existing finite-thread behavior and a guest thread that yields/resumes multiple times.

### Task 3: Waitable objects and deadlines

**Files:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-rt/src/hle/kernel32.rs`,
`crates/anywasm-rt/src/hle/mod.rs`, `crates/anywasm-rt/tests/run_exe.rs`, guest fixtures.

- [x] Model event, mutex, and semaphore state in their handles; implement signal/release operations.
- [x] Implement blocking `WaitForSingleObject` and `WaitForMultipleObjects` with wait-any/all,
  timeout-zero, finite timeout, and infinite timeout behavior.
- [x] Implement `Sleep` and `SwitchToThread` as scheduler operations driven by virtual time.
- [x] Add guest cases for auto/manual-reset events, semaphore counts, waits on thread exit, and
  timeout results.

### Task 4: Locks and condition variables

**Files:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-rt/src/hle/kernel32.rs`,
`crates/anywasm-rt/tests/run_exe.rs`, guest fixtures.

- [x] Track recursive critical-section ownership and SRW shared/exclusive ownership by guest
  address; block contending threads and wake them on release.
- [x] Implement condition-variable wait queues; release and reacquire the requested lock around a
  wait, with wake-one, wake-all, and timeout behavior.
- [x] Add the worker-waits/worker-wakes regression and cover lock reacquisition after both wake and
  timeout.

### Task 5: Target and host verification

**Files:** `docs/TARGETS.md`, `docs/STATUS.md`, `CLAUDE.md`, `test/wasm_smoke.mjs`.

- [x] Run `cargo test --workspace` and `node test/wasm_smoke.mjs` and record complete results.
- [x] Run the selected static-CRT GLFW Demo with a bounded instruction budget; record the next
  reached import/stop and whether a host window, renderer, or frame was actually observed.
- [x] Update the runtime task queue only for behaviors covered by the tests and target trace.

## Reconciliation (2026-10-11)

The planned scheduler behaviors are implemented and covered by `cargo test --workspace` and
`node test/wasm_smoke.mjs` on this revision. The bounded selected-target trace reaches the D3D11
compute/draw/present path. Remaining limits are nested synchronous callback scheduling, alertable
waits, static TLS images loaded after guest-thread creation, and suspension/resumption of a
synchronous callback across the normal browser worker slice. No host-renderer or gameplay acceptance
is claimed.