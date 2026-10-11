# Representative Target Frame-Cost Measurement Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure the selected GLFW Demo's guest instructions per presented frame and compare the default cached path with opt-in MOV emission.

**Architecture:** Export the runtime's cumulative retired-instruction counter separately from the current CPU-thread counter. Add a Node benchmark that drives an `.awb` bundle to guest `Present` stops, records the cumulative counter and wall time at each stop, and reports JIT statistics. Verify it with the checked-in finite triangle fixture, then run it against the externally built GLFW Demo bundle.

**Tech Stack:** Rust C ABI (`anywasm-wasm`), JavaScript ES modules, Node.js, existing `.awb` packer, WebAssembly runtime.

**Spec:** `CLAUDE.md`, section 1 (measure instructions per frame of a representative program before selecting performance work).

## Global Constraints

- Never speculate about unverified APIs, binaries, or measurements; retain the exact command, runtime mode, and limits with recorded results.
- Never weaken a test to make it pass.
- Missing guest imports must continue to stop loudly with `Stop::Unimplemented`.
- Develop and push only on `claude/upbeat-franklin-c1vbxr`; do not open a PR.
- Keep the measured game binary and bundle outside this repository.

## Review Focus

- A process with several guest threads reports cumulative retired instructions across threads.
- A single-thread fixture preserves the existing `instructions()` API behavior.
- A `Present` stop is counted as a frame boundary without dropping its command stream before the benchmark consumes it.
- A target that exits, idles, crashes, or stops at an unimplemented import before the requested frame count reports that terminal state.
- A benchmark instruction cap prevents an unbounded game loop from running forever.

---

### Task 1: Expose the aggregate counter and verify it on a threaded fixture

**Files:**
- Modify: `crates/anywasm-wasm/src/lib.rs`
- Modify: `web/anywasm.js`
- Modify: `test/wasm_smoke.mjs`

**Interfaces:**
- Consumes: `Process.retired_instructions`, existing `AnyWasm.instructions()`, and `thread_sync_test.exe`.
- Produces: WASM export `aw_retired_instructions() -> u64` and JavaScript method `AnyWasm.retiredInstructions() -> number`.

- [x] **Step 1: Add the failing smoke assertion**

Run `thread_sync_test.exe` through the existing WASM smoke helper and assert that `retiredInstructions()` is a finite number greater than `instructions()`.

- [x] **Step 2: Run `node test/wasm_smoke.mjs` and verify only the new aggregate-counter assertion fails because the API does not exist yet.**

- [x] **Step 3: Add the C ABI export returning `Process.retired_instructions`, or zero before process load, and the `AnyWasm.retiredInstructions()` wrapper method.**
- [x] **Step 4: Rebuild the release WASM and run `node test/wasm_smoke.mjs`; the threaded assertion must pass and existing output/count checks must remain unchanged.**
- [x] **Step 5: Commit the passing regression and implementation.**

### Task 2: Add a reusable frame-cost benchmark

**Files:**
- Create: `test/target_frame_bench.mjs`
- Create: `test/target_frame_bench_test.mjs`
- Test: `fixtures/d3d11_triangle.exe`

**Interfaces:**
- Consumes: positional `.awb` path; optional frame count and maximum guest-instruction budget; `ANYWASM_JIT_MIN_PREFIX` (default `65`).
- Produces: JSON containing each frame's cumulative retired-instruction delta and elapsed milliseconds, terminal status if reached early, total guest count, and JIT statistics.

- [x] **Step 1: Implement argument validation and release-WASM / `.awb` loading.**
- [x] **Step 2: Run in bounded slices until the requested number of `Status.FRAME` stops, a terminal status, or the instruction cap. Drain GPU bytes after each frame.**
- [x] **Step 3: Pack `fixtures/d3d11_triangle.exe`, request four frames, and verify exactly three frame records followed by clean process exit; verify the instruction cap stops before the first frame.**
- [x] **Step 4: Run `node --check test/target_frame_bench.mjs` and `node --check test/target_frame_bench_test.mjs`.**

### Task 3: Record the representative target comparison

**Files:**
- Modify: `docs/STATUS.md`
- Modify: `docs/TARGETS.md`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: target benchmark JSON, target build provenance in `docs/TARGETS.md`, and the current task queue in GitHub issues #1 and #5.
- Produces: reproducible target frame counts, separate cold-start and steady-frame data, cached-only versus forced-emitter comparison, and explicit guest-only limitations.

- [x] **Step 1: Run the benchmark against the D3D11-only `/MT` GLFW Demo bundle with the default prefix and with prefix `1`, using matching frame and instruction limits.**
- [x] **Step 2: Record command, Node/runtime version, frame counts, elapsed time, module compilations/executions, and whether any run ended before the requested frames.**
- [x] **Step 3: Update the JIT priority notes and queue issue #1/#5 based on the measured result; do not claim host rendering or game acceptance.**
- [x] **Step 4: Run `git diff --check`, `cargo test --workspace`, `cargo build -p anywasm-wasm --target wasm32-unknown-unknown --release`, `node test/wasm_smoke.mjs`, and the fixture frame benchmark.**

### Task 4: Correct retirement and callback budget accounting

**Review finding:** synchronous `call_guest` callbacks were outside the aggregate counter and could
run beyond an individual `Process::run` slice; yielding HLE calls and completed instructions before
a later block fault were also omitted.

- [x] Add regressions for a callback plus frame HLE, a nonterminating callback at the active budget,
      a block fault after one completed instruction, and a stale WASM module without the aggregate
      export. The callback-count, block-fault, and stale-export regressions were observed failing
      before their fixes; the budget regression passes against the bounded callback implementation.
- [x] Count successful nested callback instructions, HLE thunk instructions, and completed block
      instructions even when a later instruction faults. Keep `instructions()` as the active CPU's
      `icount` and expose the process aggregate only through `retiredInstructions()`.
- [x] Bound synchronous callbacks by the active `run` budget. Fail with a diagnostic if the callback
      consumes that budget before returning, because its host-stack continuation cannot be resumed.
      Make the benchmark's default slice equal its requested total cap; smaller explicit slices can
      encounter this documented runtime limitation.
- [x] Require `aw_retired_instructions` in the JS API instead of silently falling back to
      `aw_icount` for stale WASM builds.
- [x] Rebuild the release WASM, pass the workspace and WASM smoke suites, and rerun the fixture and
      representative target benchmarks. Record the corrected results in `CLAUDE.md`,
      `docs/STATUS.md`, `docs/TARGETS.md`, and GitHub issues #1 and #5.

