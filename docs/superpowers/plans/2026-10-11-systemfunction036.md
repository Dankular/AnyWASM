# SystemFunction036 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve the selected GLFWDemo's late-bound `advapi32!SystemFunction036` call with a secure provider on native and WASM hosts.

**Architecture:** Add an `advapi32` HLE that stages all bytes before writing guest memory. Native builds use the operating system's secure random source; WASM requests bytes from the JS host through `anywasm.random_fill`, backed by Web Crypto.

**Tech Stack:** Rust, Windows BCrypt, Unix `/dev/urandom`, WebAssembly C ABI host import, JavaScript Web Crypto, freestanding Win64 fixture.

**Spec:** [2026-10-11-systemfunction036-design.md](../specs/2026-10-11-systemfunction036-design.md)

## Global Constraints

- Never substitute the deterministic process PRNG for cryptographic entropy.
- Return `FALSE` when entropy or allocation fails, and do not write partial output to guest memory.
- Keep the guest's Win64 ABI and AnyWASM C-ABI exports unchanged.
- Target success is only progression beyond the current fatal path; no window, renderer, or frame is implied.

## Review Focus

- Zero-length request with a null guest pointer: succeeds without touching a host provider.
- Host entropy unavailable: returns failure and leaves the guest buffer unchanged.
- Web Crypto's 65,536-byte per-call limit: host chunks large buffers without exceeding it.
- Invalid host linear-memory range: host provider returns failure rather than throwing through the WASM call.
- Native OS source failure or allocation failure: HLE returns failure without partial guest writes.

### Task 1: `advapi32` HLE and native provider

**Files:**
- Modify: `crates/anywasm-rt/src/hle/mod.rs`
- Create: `crates/anywasm-rt/src/hle/advapi32.rs`
- Test: `crates/anywasm-rt/src/hle/mod.rs` and `crates/anywasm-rt/src/hle/advapi32.rs`

**Interfaces:**
- Consumes: `Process::arg`, `Process::write_bytes`, `HleFn`, and the existing `advapi32` group mapping.
- Produces: `lookup("advapi32", "SystemFunction036") -> Some(HleFn)` and a platform provider that fills a Rust byte slice or reports failure.

- [x] Add a lookup regression and a handler behavior regression for successful, zero-length, and failed-provider calls.
- [x] Run the focused tests and confirm they fail because the export/provider behavior is missing.
- [x] Implement the HLE with staged output; use BCrypt on Windows and `/dev/urandom` on Unix-like native hosts.
- [x] Run focused tests, then `cargo test -p anywasm-rt`.

### Task 2: WebAssembly host entropy bridge and fixture

**Files:**
- Create: `fixtures/system_random_test.c`
- Modify: `fixtures/build.sh`
- Modify: `crates/anywasm-rt/src/hle/advapi32.rs`
- Modify: `web/anywasm.js`
- Modify: `test/wasm_smoke.mjs`
- Modify: `test/browser/run.mjs`
- Create: `web/entropy.js`

**Interfaces:**
- Consumes: the HLE's staged-buffer provider from Task 1.
- Produces: `anywasm.random_fill(ptr: i32, len: i32) -> i32`, returning 1 only after all bytes are filled.

- [x] Add a freestanding executable that loads `advapi32.dll`, resolves `SystemFunction036` with `GetProcAddress`, calls zero-length and nonzero requests, and add smoke assertions.
- [x] Build the fixture and verify the native runtime trace exits with code 0.
- [x] Add the WASM `anywasm.random_fill` import and implement the JS callback with Web Crypto in 65,536-byte chunks.
- [x] Run Node WASM smoke and browser fixture checks.

### Task 3: Target replay and records

**Files:**
- Modify: `docs/TARGETS.md`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: the updated HLE and both entropy providers.
- Produces: an exact bounded-run result and an updated outstanding-work record.

- [x] Rerun the selected GLFWDemo with `--trace-imports --max-steps 30000000` and the Release directory as VFS root.
- [x] Record the progression through `RtlPcToFileHeader` to the undispatched C++ `RaiseException`; no frame or renderer is claimed.
- [x] Run final workspace tests, release WASM build, Node smoke, and browser suite after the unwind primitive changes.

