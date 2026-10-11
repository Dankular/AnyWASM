# Per-Thread Static TLS Implementation Plan

> **For the implementer:** Keep the TEB address shared, but make the TLS vector and blocks private
> to each guest thread.

**Goal:** Initialize PE static TLS independently for each cooperative guest thread and fix the
selected GLFWDemo slot-zero fault.

**Design:** `docs/superpowers/specs/2026-10-11-static-tls-per-thread.md` records the target trace,
index ownership, scheduler data flow, and unsupported dynamic-load boundary.

**Changes:** `crates/anywasm-rt/src/process.rs`, `crates/anywasm-rt/src/hle/kernel32.rs`,
`crates/anywasm-rt/tests/run_exe.rs`, and a freestanding fixture source/build entry.

### Task 1: Add the guest regression first

- [x] Add a `__declspec(thread)` fixture that checks the main template, worker template, and
      isolation after worker mutation.
- [x] Build and run it against the current runtime; the first run faulted on the null TLS vector
      before `main`, confirming the loader had not initialized static TLS.

### Task 2: Model module TLS templates and per-thread vectors

- [x] Store static TLS index, initialized bytes, and total size for each TLS-bearing module.
- [x] Assign static TLS indexes starting at zero, initialize the main vector, and clone blocks when
      creating a guest thread.
- [x] Save/restore each thread's `TEB+0x58` vector pointer with its stack and last-error fields.
- [x] Reject loading a TLS-bearing module after guest threads have been created.

### Task 3: Separate TLS and FLS allocation indexes

- [x] Give static TLS, `TlsAlloc`, and `FlsAlloc` independent index counters, each starting at zero.
- [x] Add unit coverage for separate TLS/FLS index spaces and static TLS vector/block isolation.
- [x] Run the guest fixture and scheduler regressions.

### Task 4: Verify target progress

- [x] Run `cargo fmt --all`, `cargo test --workspace`, and the WASM smoke test.
- [x] Rerun the selected target with the same 30,000,000-instruction budget and import tracing.
- [x] Record the actual next stop, and keep window/renderer/game claims tied to observed evidence.
