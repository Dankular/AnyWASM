# Shared Memory Feasibility Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure the current full-runtime shared-memory limitation and prove a safe no-std CPU-worker module shape over one bounded shared memory.

**Architecture:** Build one full-runtime-shaped probe with mutable statics and a stand-in cursor value, plus a no-std worker probe with explicit context offsets and an exported per-instance stack pointer. A Node harness instantiates workers over one bounded `WebAssembly.Memory` and measures state aliasing, atomic visibility, worker overlap, stack separation, and guest-data preservation. It also attempts the production runtime build and Asyncify pass. This is a gate only; it does not enable parallel guest execution.

**Tech Stack:** Rust `wasm32-unknown-unknown`, WebAssembly shared memory and atomics, Node.js, Binaryen Asyncify.

**Spec:** `docs/superpowers/specs/2026-10-12-sharedarraybuffer-guest-threads-design.md`

## Global Constraints

- The existing single-worker path remains buildable and runnable.
- Shared Wasm memory has an explicit maximum.
- The coordinator remains the only owner of process-wide state.
- No worker repeats initialization over live shared runtime state.
- CPU workers do not allocate or mutate process statics; each worker context and stack occupies a disjoint explicit region.
- The production shared-memory worker path remains disabled until a CPU core and shared guest-memory backend are integrated.

## Review Focus

- Each module imports shared memory with a maximum compatible with the host memory object; the harness instantiates the modules successfully.
- A Rust mutable static is shared across instances; the harness checks that writing through instance A is observed by instance B.
- A stand-in cursor value in shared linear memory advances across instances; the probe does not claim to measure `std`'s allocator implementation.
- Atomic updates to shared guest bytes are visible and count exactly once; the harness runs two workers and verifies the final count and overlap.
- Worker stack pointers are distinct and initialized data stays below the shared guest region; the harness checks low-boundary canaries, expected context checksums, and a guest-data sentinel.
- The full runtime build and Asyncify attempt report whether the installed production toolchain supports shared memory without replacing the normal artifact.

---

### Task 1: Build a reproducible shared-memory ABI probe

**Files:**
- Create: `test/fixtures/wasm_shared_memory_probe.rs`
- Create: `test/fixtures/wasm_shared_worker_probe.rs`
- Create: `test/wasm_shared_memory_probe.mjs`
- Modify: `docs/superpowers/specs/2026-10-12-sharedarraybuffer-guest-threads-design.md` to record the measured feasibility result and required architecture revision.

**Interfaces:**
- Produces a temporary `.wasm` module importing one shared memory with a fixed maximum.
- Exports `atomic_increment() -> u32`, `set_runtime_state(u32)`, `runtime_state() -> u32`, and `stand_in_cursor_probe() -> u32`.
- The no-std module exports `worker_slice(context_address, counter_address, active_address, maximum_active_address, seed) -> u32` and the instance-local `__stack_pointer` global.
- The Node harness reports expected design blockers while exiting nonzero for probe build, import, lost atomic update, lost worker overlap, context corruption, or guest-data corruption.

- [x] **Step 1: Add the Rust probes**: one with atomic guest data, mutable runtime state, and a stand-in cursor; one no-std worker module with no allocator or mutable static and a configurable stack pointer.
- [x] **Step 2: Add the Node harness** to compile both probes with `+atomics,+bulk-memory,+mutable-globals`, `--import-memory`, `--shared-memory`, and a bounded maximum; instantiate multiple modules over the same `WebAssembly.Memory`.
- [x] **Step 3: Pin concurrency and isolation** by asserting exact atomic totals, worker overlap, per-worker context values, unique stack regions, and guest-data preservation; report mutable-static and stand-in cursor aliasing.
- [x] **Step 4: Build the production Wasm crate with the same shared-memory linker settings** in a dedicated target directory and run the existing Asyncify transform if linking succeeds; report the linker reason when it does not.
- [x] **Step 5: Record the gate result** in the spec. If the full-runtime build or state isolation fails, keep that layout disabled and continue only with a coordinator/CPU-worker split whose worker state is explicit and disjoint.
- [x] **Step 6: Verify** with `node test/wasm_shared_memory_probe.mjs`, `node test/wasm_shared_memory_probe.mjs --runtime`, and `git diff --check`.
