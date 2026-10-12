# Dynamic WASM JIT completeness and performance plan

> **For agentic workers:** Use the `superpowers:executing-plans` workflow. Implement each checkbox
> test-first, run the listed focused verification, and keep generated build artifacts out of commits.

**Goal:** Expand the opt-in emitted path's memory, exit, SSE, and x87 coverage; compare complete CPU
state against interpreter execution; and demonstrate repeatable memory plus FP/control-flow gains.

**Architecture:** Keep `BlockCache` and the interpreter authoritative. Generated WASM works from a
complete copied CPU snapshot, validates every inline memory access, and commits only a valid
retired-count result. JavaScript chains only among live registered starts. Unsupported instructions
or unsafe domains return to the interpreter. Benchmark the same release WASM build in cached-only
and forced-emission modes.

**Spec:** `docs/superpowers/specs/2026-10-12-jit-completeness-design.md`.

## Constraints

- Do not commit or push. Preserve existing unrelated edits in `crates/anywasm-rt/src/process.rs`,
  `crates/anywasm-rt/src/hle/d3d9.rs`, and `target-trace/`.
- Keep dynamic emission opt-in unless same-build results establish a broader win.
- Do not bypass guest permission checks, code-epoch invalidation, retired-count accounting,
  HLE/yield/stop handling, or interpreter fault behavior.
- Compare f64 x87 state bitwise. Support the current interpreter's f64 x87 model; do not claim
  architectural 80-bit precision.
- The browser harness currently passes the JIT and memory-growth assertions but ends with one
  separate D3D11 structured-buffer UAV shader-validation failure. Report these outcomes separately.

## Task 1: Complete the JIT CPU snapshot and add differential inspection

**Files:** `crates/anywasm-cpu/src/jit.rs`, `crates/anywasm-wasm/src/lib.rs`, `crates/anywasm-wasm/Cargo.toml`,
`web/anywasm.js`, `test/wasm_smoke.mjs`.

- [x] Add failing unit assertions proving the snapshot round-trips non-default `CpuMode`, FS/GS,
  GPRs, flags, XMM/YMM, MXCSR, all x87 slots/TOP/CW/SW/TW, RIP, TSC, and icount.
- [x] Add a test-only WASM snapshot export behind a dedicated Cargo feature and a JS helper that
  serializes all fields into a stable byte layout; assert the export is present in the test build
  and absent from the normal artifact if the build setup supports separate artifacts.
- [x] Build deterministic fixture runs for interpreter, cached-only, and forced emission. Assert
  equal status, exit code, stdout/stderr, retired count, and complete CPU snapshot; compare all
  fixture-touched memory ranges.
- [x] Expand `JitState`, offset/size assertions, `From<&Cpu>`, and commit copy-back to include every
  architectural CPU field. Keep execution metadata separate from the CPU snapshot.
- [x] Run `cargo test -p anywasm-cpu`, the feature-enabled WASM build, and focused Node differential
  checks. Verify the expanded snapshot remains unchanged after a rejected/trapped emitted call.

## Task 2: Measure and implement a multi-page emitted-memory fast path

**Files:** `crates/anywasm-cpu/src/mem.rs`, `crates/anywasm-cpu/src/jit.rs`,
`test/wasm_smoke.mjs`, `test/browser/run.mjs`.

- [ ] Add failing memory tests for hot accesses across at least four guest pages, read/write
  permissions, map/unmap/protect invalidation, boundary crossing, executable-page stores, and
  fault fallback. Include a memory-growth-after-emission browser check if needed to keep it
  independent from the existing compute fixture check.
- [ ] Capture a same-build benchmark baseline for memory and the other four fixtures with at least
  ten measured rounds, one warm-up, and correctness checks on output and retired count.
- [ ] Implement a compact multi-entry direct-mapped or MRU-plus-set TLB shared by the interpreter
  and emitted path. Retain the existing single-page hot hit when it benchmarks better. Inline
  probes must check page identity, permissions, within-page access, and frame bounds.
- [ ] Run focused Rust and WASM/browser JIT checks. Keep the new TLB only if the paired memory
  throughput improves without any correctness regression; otherwise retain the tests and use the
  measured result to select a different memory fast path.

## Task 3: Add indirect register JMP exits and harden chained handoff

**Files:** `crates/anywasm-cpu/src/jit.rs`, `web/anywasm.js`, `test/wasm_smoke.mjs`,
`fixtures` and its build script if a new guest fixture is required.

- [ ] Add failing translation and differential cases for register-indirect JMP to a registered
  block, to an unregistered block, to an HLE thunk region, and to a noncanonical/unmapped address.
- [ ] Emit the register target into architectural RIP and end the emitted block. Let the existing
  registry chain a known target; on a miss, return to normal Rust block lookup/interpreter handling.
- [ ] Verify conditional/direct chaining still preserves flag reads, instruction budgets, retired
  counts, and code-epoch invalidation. Confirm unresolved control flow never executes stale bytes.
- [ ] Run focused CPU, WASM, browser JIT, and cache-pressure checks.

## Task 4: Extend guarded SSE emission

**Files:** `crates/anywasm-cpu/src/jit.rs`, `test/wasm_smoke.mjs`, `test/browser/run.mjs`,
`fixtures` and its build script if needed.

- [ ] Add failing interpreter-vs-emitted differential cases for selected packed-single and packed-
  double arithmetic beyond ADD/MUL, including finite normal values, signed zero, subnormal,
  infinity, NaN, MXCSR controls, and memory operands.
- [ ] Add only operations whose normal finite results match the interpreter's rounding and whose
  exceptional/control cases can reliably guard to fallback. Reuse full XMM state snapshots.
- [ ] Verify unsupported domains fall back from an untouched CPU/memory state and report exact
  output and retired counts.
- [ ] Run Rust, WASM, Chromium page/worker JIT, and cached-only/emitted differential checks.

## Task 5: Emit a narrow f64-compatible x87 stack subset

**Files:** `crates/anywasm-cpu/src/jit.rs`, `test/wasm_smoke.mjs`,
`fixtures` and its build script if needed.

- [ ] Add failing cases for register-stack FLD/FXCH/FSTP and basic ST arithmetic forms that are
  already represented directly in `crates/anywasm-cpu/src/x87.rs`.
- [ ] Emit only the selected f64-equivalent operations. Preserve TOP, CW, SW, TW, all stack slots,
  and the interpreter's count/RIP behavior; leave memory forms, environment operations,
  transcendentals, and any unsupported exception/tag case interpreted.
- [ ] Compare complete CPU snapshots for each x87 case and verify fallback cases do not partially
  change stack or flags.
- [ ] Run CPU, WASM differential, and browser JIT checks.

## Task 6: Final paired benchmark and status reconciliation

**Files:** `test/bench.mjs`, `CLAUDE.md`, `docs/STATUS.md`, `docs/superpowers/specs/2026-10-12-jit-completeness-design.md`.

- [ ] Add warm-up and paired-round support to the benchmark without changing its default opt-in
  behavior. Report medians and emitted-module, execution, chain, and fallback counts.
- [ ] Run at least ten measured paired rounds per workload on one release WASM build. Confirm
  output, errors, exit code, and retired counts are identical before accepting a comparison.
- [ ] Require at least 10% median improvement on the memory fixture and one FP/control-flow
  fixture. If that bar is not met, continue optimizing or report the objective as incomplete; do
  not claim a performance gain from noisy or guest-only target measurements.
- [ ] Reconcile current JIT checklist state, benchmark methodology/results, and known limitations in
  `CLAUDE.md` and `docs/STATUS.md`. Keep the separate browser graphics failure accurately reported.
- [ ] Run `git diff --check`, `cargo test --workspace`, release WASM build, `node test/wasm_smoke.mjs`,
  and the focused browser JIT checks. Run the full browser suite and report unrelated failures
  separately.
