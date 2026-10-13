# Parallel SharedArrayBuffer Guest Threads Implementation Plan

> **Execution:** inline implementation, authorized by the user's “work on SharedArrayBuffer” / “DO IT” direction. This plan is the concrete work breakdown for the existing design spec.

**Goal:** Run at least two ready Win64 guest threads concurrently in separate browser CPU workers against one shared guest address space, while the existing process coordinator serializes HLE, scheduling, mapping changes, exceptions, and process-wide effects. PE32 shared-worker execution stays single-context while its legacy fixed TEB alias is in use.

**Architecture:** Keep one `anywasm-wasm` process coordinator. Extend the current `SharedCpuWorkerPool` handoff so it can dispatch independent ready contexts together, then collect every safe-point result before coordinator work. Give each worker context isolated TEB/TLS state, shared atomic guest-frame access, per-thread instruction accounting, and explicit completion reasons. The default browser and CLI paths remain cooperative.

**Tech Stack:** Rust `anywasm-cpu-state` / `anywasm-cpu-worker` / `anywasm-rt` / `anywasm-wasm`, browser ES modules, Node worker tests, freestanding Win64 fixtures, existing browser replay harness.

**Spec:** `docs/superpowers/specs/2026-10-12-sharedarraybuffer-guest-threads-design.md`

## Constraints

- Keep `?sharedCpuWorkers` opt-in until the acceptance criteria pass. Preserve cooperative fallback when capability checks or pool initialization fail before guest execution.
- The process coordinator is the only owner of `Process`, handles, HLE, module lifecycle, waits, virtual time, exception dispatch, GPU/output, and mapping changes.
- CPU workers receive explicit contexts and offsets. They must not construct or mutate `Process`, Rust allocator state, or process-global statics.
- Every concurrently accessed guest frame byte must use the shared atomic access layer. Publish mapping and code generations only after all workers are quiescent.
- A worker failure after guest writes have begun is fatal unless it published a complete context and a defined completion record.
- Keep the ordinary non-threaded Wasm artifact and CLI cooperative scheduler behavior unchanged.
- Do not claim general Diligent rendering, x87 coverage, or target acceptance from pool startup or a synthetic overlap fixture alone.

## Review Focus

- No thread is dispatched while blocked, suspended, exited, or already owned by another worker.
- Each guest thread retains independent GPR/SIMD/x87 state, segment bases, stack, TEB fields, TLS/FLS, last error, exception state, and retired-instruction count.
- Parallel slices share guest writes, but HLE, waits, mapping changes, DLL loads, exceptions, and process exit have a single coordinator serialization point.
- Parallel budgets preserve both per-thread quanta and the configured process-wide instruction limit.
- Every HLE/fault/exit completion returns a complete context before coordinator effects are applied.
- Pool startup and mid-run failures cannot leave workers writing after shared memory is released or reused.

---

### Task 1: Make coordinator context ownership explicit

**Files:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-wasm/src/shared_context.rs`, `web/anywasm.js`

- [x] Add coordinator queries that enumerate only runnable saved contexts plus the currently running context; include thread state and a stable per-thread quantum/accounting value.
- [x] Add a safe-point import path that stores a returned context for its owning thread without incorrectly making a saved thread the active process thread.
- [x] Add a coordinator operation that selects one completed thread for serialized HLE/fault handling and restores the existing scheduler state afterward.
- [x] Preserve the existing active-thread API for the serial mode until the parallel path is enabled.
- [x] Define the browser budget contract: one 250,000-instruction allowance per worker-loop pump, charged across coordinator `run` calls and worker slices; a batch's reservations must sum to the remaining allowance, and actual worker retirements return unused reservation before the next batch.

**Acceptance:** A context can move from coordinator-owned Ready/Running state to worker-owned and back exactly once, while blocked/exited contexts remain undispatchable and all returned instruction deltas are charged once.

**Budget contract:** The browser worker owns a single 250,000-instruction allowance for each outer worker-loop pump. It subtracts coordinator-retired instructions and returned worker instruction deltas from that same allowance. `dispatchBatch` receives the remaining allowance and reserves no more than its total; each reservation is bounded by the context's remaining thread quantum. Unused reservation becomes available after all contexts in the batch return. A later UI pump begins a new allowance, matching the existing browser's repeated bounded `w.run(250_000)` slices.

### Task 2: Isolate guest TEB and TLS state for concurrent execution

**Files:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-rt/src/hle/kernel32.rs`, `crates/anywasm-rt/src/hle/ntdll.rs`, `crates/anywasm-wasm/src/shared_context.rs`, `crates/anywasm-cpu-state/src/shared.rs`

- [x] Allocate or assign one guest TEB page and TLS-vector storage per Win64 guest thread, keeping each page in the common guest mapping and shared-frame snapshot.
- [x] Export worker CPU contexts with the correct per-thread FS/GS base; never let two workers use the coordinator's fixed `TEB_ADDR` as their live segment base.
- [x] At a coordinator HLE boundary, materialize the selected thread's TEB at the existing HLE alias, perform the call, and copy coordinator changes back to that thread's TEB before worker resumption.
- [x] Preserve guest-written TEB fields and per-thread exception, stack, last-error, TLS/FLS, and thread-ID values across worker/coordinator transitions.
- [x] Make TEB/TLS mapping allocation failure occur before dispatch or return a defined guest allocation failure.

**Acceptance:** Two concurrent Win64 contexts repeatedly read and write distinct TEB/TLS sentinels, then pass through serialized HLE and recover their own values with no cross-thread leakage.

### Task 3: Dispatch multiple contexts and collect completions

**Files:** `web/shared-cpu-pool.js`, `web/worker.js`, `web/shared-cpu-worker.js`, `crates/anywasm-cpu-state/src/shared.rs`, `crates/anywasm-cpu-worker/src/lib.rs`

- [x] Add a batch dispatch API that reserves distinct idle worker slots and distinct thread contexts before sending any work.
- [x] Bound each reservation by the thread's remaining scheduler quantum and the coordinator's remaining process budget.
- [x] Run ready contexts concurrently with `Promise.all`-style completion collection; preserve the existing single-worker dispatch behavior for fallback and comparison.
- [x] Import all complete contexts and dirty pages only after every dispatched worker reaches a safe point.
- [x] Route HLE, fault, and thread-exit completions through the coordinator in deterministic sequence order; reject worker failures after every dispatched slice has settled and consume completed slots before reuse.
- [x] Add stop/dispose handling that waits for bounded worker slices to reach a safe completion before releasing pool state.

**Acceptance:** Two workers overlap execution using separate slots and contexts; mismatched sequence IDs, duplicate thread ownership, partial completion, and worker errors are rejected without dispatching additional work.

### Task 4: Define shared-memory, mapping, and code-update barriers

**Files:** `crates/anywasm-cpu-state/src/guest_memory.rs`, `crates/anywasm-cpu-state/src/shared.rs`, `crates/anywasm-cpu-worker/src/lib.rs`, `crates/anywasm-rt/src/process.rs`, `crates/anywasm-wasm/src/shared_snapshot.rs`, `web/shared-cpu-pool.js`

- [x] Audit every shared guest load/store and locked instruction for atomic access and documented alignment behavior, including HLE guest-buffer reads/writes.
- [x] Ensure every guest page mutation, permission change, DLL mapping, executable write, and unmap first reaches an all-worker quiescence barrier.
- [x] Publish mapping/code/dispatch generations atomically and make each worker stop and refresh mappings or invalidate translated code before its next slice.
- [x] Keep dirty-page collection correct when more than one worker writes different or identical pages in the same batch.
- [x] Make unsupported atomic widths and alignment faults explicit and consistent with the current worker ISA limits. Shared-memory faults preserve their unsupported category through the executor ABI, publish a dedicated worker stop reason, and leave the instruction at its original RIP for coordinator-interpreter fallback.

**Acceptance:** No worker can use a stale mapping or translated block after a generation change; shared writes survive collection, and locked operations have one documented linearization point.

### Task 5: Complete worker instruction and stop-reason behavior

**Files:** `crates/anywasm-cpu-state/src/exec_core.rs`, `crates/anywasm-cpu-state/src/x87.rs`, `crates/anywasm-cpu-worker/src/lib.rs`, `crates/anywasm-cpu-worker/Cargo.toml`, `crates/anywasm-rt/src/exception.rs`

- [x] Bring x87 instructions needed by the selected fixture into the no-std worker build, or stop unsupported x87 instructions at their original RIP for coordinator-owned interpreter fallback before state is lost.
- [x] Verify the serialized CPU ABI carries all architectural state used by the worker, including x87, flags, FS/GS bases, instruction count, and stop metadata.
- [x] Distinguish quantum, HLE, guest fault, thread exit, and worker failure; never advance RIP for an instruction that did not retire. Win32 scheduling yields are requested by serialized HLE and now remain effective across a saved-context handoff, instead of restoring the previous thread directly. The worker ABI also distinguishes process/thread return trampolines and retries unsupported atomic operations at their original RIP.
- [x] Hand complete faulting contexts to existing coordinator exception dispatch and resume only from its resulting context.
- [ ] Keep decoded/JIT state worker-local and enforce generation checks before each translated block if JIT execution is enabled.

**Acceptance:** The worker never loses a context at an unsupported instruction, exception, HLE call, or thread exit, and its reported retired count matches the returned context.

### Task 6: Add concurrent guest and browser regression coverage

**Files:** `test/shared-cpu-pool.test.mjs`, `crates/anywasm-rt/tests/run_exe.rs`, `fixtures/`, `test/wasm_smoke.mjs`, `web/main.js`, `web/worker.js`

- [x] Add shared-pool unit coverage for two distinct contexts in separate slots/context offsets, concurrent request dispatch, process-wide budget reservation, completion markers, and duplicate-context rejection (`test/shared-cpu-pool.test.mjs`).
- [x] Keep the two-thread Chromium fixture and no-std overlap probe as separate evidence: `test/browser/parallel.mjs` checks a two-context batch plus per-thread/locked-total results; `test/wasm_shared_memory_probe.mjs` directly measured maximum worker concurrency of two and checked atomic totals, context checksums, and stack guards.
- [ ] Add a Win64 guest fixture that creates two threads, exercises TLS/TEB isolation, Win32 synchronization and interlocked shared state, joins both threads, and checks a deterministic result.
- [x] Cover global budget reservation, dirty-page/mapping resync, worker failure during work, pool disposal, and rejection when cross-origin isolation is unavailable (`test/shared-cpu-pool.test.mjs`).
- [x] Cover runnable-state filtering and the PE32 active-context restriction (`process::scheduler_tests`).
- [ ] Cover serialized HLE/fault handoff, Worker startup failure after partial initialization, and cooperative fallback after startup failure.
- [x] Report selected execution mode and a specific fallback/failure reason in the browser status path.
- [ ] Keep any test-only tracing behind explicit flags; do not ship hard-coded address or target-specific debug traces in the default path.

**Acceptance:** The browser fixture proves overlap rather than merely creating multiple workers; all fallback/error cases leave the cooperative runtime usable when no worker execution began.

### Task 7: Replay the selected target and reconcile scope

**Files:** `test/wasm_smoke.mjs`, `docs/TARGETS.md`, `docs/STATUS.md`, `CLAUDE.md`

- [ ] Run the full Rust workspace suite, shared-pool tests, Wasm smoke suite, and a cross-origin-isolated browser replay.
- [ ] Replay the selected Diligent target with a bounded process budget and record active worker count, concurrent overlap, import/stop reason, shader/present counters, and captured pixels separately.
- [ ] Compare against the cooperative baseline at the same limits and report the next observable milestone or the exact blocker.
- [ ] Update target and status documentation with only behaviors directly established by the fixtures and replay.

**Acceptance:** Synthetic concurrency, guest synchronization, browser fallback, and target progress are reported as separate results; target rendering is claimed only when browser pixels establish it.

## Verification commands

Run after implementation tasks are complete (not during planning):

```powershell
cargo test --workspace
bash web/build.sh
node --test test/shared-cpu-pool.test.mjs
node test/wasm_smoke.mjs
```

Then run the project's existing cross-origin-isolated browser replay and selected bounded target replay, preserving their logs/captures under `target/` and updating the target ledger.

## Self-review notes

- The existing pool already supports multiple worker slots at the low-level `dispatch(threadId, budget)` API. Remaining work is acceptance coverage, complete fault/stop-reason review, and target replay.
- The coordinator keeps one fixed `TEB_ADDR` HLE view; each Win64 worker context uses a unique per-thread TEB backing page and coordinator handoffs copy through that alias.
- PE32 guest threads retain the fixed TEB alias; runnable-context enumeration exposes only its current active thread to the shared pool, preventing concurrent PE32 TEB access.
- The worker ABI contains x87 state in `Cpu`. The no-std worker stops before retiring x87 instructions, and the browser coordinator retries one instruction with the std interpreter; x87 execution is therefore serialized and is not an overlap claim.
- The pool's dirty-page bitmap is page-granular. Concurrent writes to the same guest page use the atomic guest access layer; coordinator publication and mapping changes require all workers quiescent.
- The global budget contract and coordinator HLE handoff are implemented. Shared-worker mode remains opt-in until concurrent fixtures and browser replay establish guest-level behavior.
