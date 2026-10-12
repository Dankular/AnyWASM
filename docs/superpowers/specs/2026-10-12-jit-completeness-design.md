# Dynamic WASM JIT completeness and performance design

**Status:** approved for implementation by the user (“always approve”).

## Verified current state

- `crates/anywasm-cpu/src/mem.rs` uses one `Cell<TlbEntry>` for reads and one for writes. The
  emitted module checks entry identity, permissions, page boundary, and backing-frame bounds.
  Mapping, unmapping, protection changes, and executable writes preserve interpreter faults and
  self-modifying-code behavior through fallback and cache invalidation. A four-page hot-access test
  passes; the existing one-entry design remains, with no multi-entry alternative implemented.
- `crates/anywasm-cpu/src/jit.rs` emits NOP, selected 32/64-bit GPR MOV and memory MOV/ADD, 32-bit
  ADD with arithmetic flags, direct JMP/Jcc, register-indirect JMP, full-width XMM moves, guarded
  packed-double ADD/MUL/SUB/DIV, and selected register-stack x87 operations. The JS trampoline
  caches modules and chains only to registered emitted block starts. Unregistered indirect targets
  return to normal lookup/fault handling; HLE targets return to the runtime dispatcher. CALL remains
  interpreted.
- The original 624-byte `JitState` omitted mode, FS/GS bases, YMM upper halves, and x87 state.
  This task has extended it to 976 bytes and added a `jit-test-api` WASM snapshot of every CPU
  field and every mapped guest page. Full-state smoke comparisons pass on compute and
  `jit_state.exe` across emitted, cached-only, and interpreter paths.
- Additional full-state smoke fixtures exercise packed-double normal lanes, signed zero, subnormal,
  infinity, NaN, non-default MXCSR, and interpreted memory-source fallback. The x87 fixture exercises
  FLD/FXCH/FSTP and all six selected D8 ST arithmetic selectors; a separate exceptional-input fixture
  verifies fallback leaves the same final CPU and mapped memory state as cached-only/interpreter runs.
  Packed-single SSE arithmetic remains interpreter-only; this task measures and emits the guarded
  packed-double register forms.
- `Cpu` represents x87 values as f64 and separately stores TOP, control, status, and tag words.
  The emitter supports FLD/FXCH/FSTP register-stack forms and selected D8 ST arithmetic under that
  model; memory forms, extended precision, and unsupported exceptional cases remain interpreted.
- `test/browser/run.mjs` starts forced emission, grows exported memory by one page, resumes, and
  checks output and retired count against cached-only execution. The JIT page/worker and memory-growth
  checks pass. The latest full Chromium run had seven separate input/canvas assertion failures
  (mouse-coordinate scaling, mouse-leave/input-fixture exit, and screenshot extent/interpolated-pixel
  checks); its JIT and graphics assertions passed without page or WebGPU validation errors.
- A fresh same-build Node benchmark used one warm-up and 10 alternating paired rounds. Median cached
  vs emitted guest MIPS: compute 20.14/6.28 (-68.8%), call-heavy 17.32/4.31 (-75.1%), memory-heavy
  19.61/65.14 (+232.2%), SSE-float 26.35/42.74 (+62.2%), block-chain 21.12/5.57 (-73.6%). Every
  pair matched status, exit code, stdout/stderr, and retired count. Memory and SSE meet the 10% target;
  emission stays opt-in due to regressions on the other three workloads. The representative emitted
  counters are documented in `docs/STATUS.md`.

## Goal and acceptance

Make the emitted path correct across its complete declared instruction/state contract, improve
guest-memory and control-flow coverage, add a useful x87 subset compatible with the current f64
interpreter, and demonstrate repeatable same-build gains on a memory workload and at least one
floating-point or control-flow workload. Keep emission opt-in unless the broader workload set
supports enabling it by default.

Acceptance requires:

1. Interpreter and emitted runs agree on status, exit code, output, errors, retired count, every
   represented CPU field, and all mapped guest-memory bytes for deterministic differential
   fixtures. A mismatch is a failure; unsupported cases must fall back from an untouched snapshot.
2. Memory tests cover mapped hits, multiple hot pages, read/write permissions, remap/protect
   invalidation, page-boundary accesses, memory growth, executable-page writes, and interpreter
   faults. Any multi-entry TLB change is kept only if paired benchmarks justify it.
3. Direct and indirect exits preserve RIP, flags, stack, count, and budget semantics. Calls or
   exits into unregistered code, HLE thunks, or runtime stop/yield handling return control to the
   interpreter/runtime without executing stale generated code.
4. Added SSE/x87 operations match the current interpreter exactly for their declared supported
   input domain and use interpreter fallback outside it. x87 state includes TOP, CW, SW, TW, and
   all eight f64 stack slots in snapshots.
5. After correctness passes, paired cached-only/forced-emission benchmarks use at least ten
   measured rounds per workload after warm-up. Report medians and JIT counts. Require at least a
   10% median gain on the memory fixture and one FP/control-flow fixture, with matching retired
   counts and output. Record any material regressions and retain opt-in default unless an explicit
   broader enablement criterion is met.

## Architecture

### State and fallback contract

Keep the interpreter as the architectural reference and generated modules as guarded fast paths.
Extend `JitState` by appending explicitly laid-out fields for omitted CPU state. Preserve and assert
the shared Rust/WASM offsets. `From<&Cpu>` must snapshot every architectural field used by the
emitter; `copy_to` must commit all such fields only after the host returns an exact, valid retired
count. Any invalid instruction, permission miss, unsupported control mode, generated-module trap,
or cache validation failure retries through the interpreter from the original live `Cpu` and
`Memory` state.

Add a test-only CPU snapshot export or equivalent harness hook so `test/wasm_smoke.mjs` can compare
all CPU fields after interpreter, cached-only, and emitted execution. Keep this hook out of the
normal public runtime API where the build system permits. Compare f64 x87 slots by bit pattern and
include YMM upper halves, MXCSR, mode, FS/GS bases, flags, counters, and x87 control/status/tag
state. Compare complete mapped-memory snapshots for the bounded deterministic fixtures.

### Guest memory

Preserve guest page permissions and fault reporting in the interpreter. Evaluate a compact
direct-mapped multi-entry TLB for emitted reads/writes while retaining a fast most-recent-page path
if it wins locally. Entries must carry guest page, backing offset, and permissions; every direct
probe validates the selected entry, access permission, page containment, and current backing-memory
length. Cache invalidation on map/unmap/protect and executable writes remains authoritative. Do not
let emitted stores continue into a later instruction until self-modifying-code invalidation is
handled safely.

### Exits and chaining

Retain direct JMP/Jcc chaining. Add indirect JMP support only when its target can be committed to
the state and resolved through the current registered-start lookup; an unresolved target ends the
chain and returns to normal block lookup/interpreter execution. Add CALL only with exact stack write,
return-address, permission/fault, and budget behavior; otherwise leave it interpreted. Never chain
through HLE thunk addresses or bypass `Stop`, `yield_request`, callbacks, or budget boundaries.

### SSE and x87

Implement only operations whose result/state behavior can be guarded against the current
interpreter. Grow SSE coverage by selected common arithmetic forms and memory operands after
differential fixtures exist. For x87, start with register-stack arithmetic and push/pop forms that
map directly to the current f64 `x87.rs` semantics; update TOP and tag state exactly. Instructions
that depend on unmodeled precision, unsupported rounding/exception behavior, memory side effects,
or transcendental approximations remain interpreted.

### Measurement

Use `test/bench.mjs` fixtures for compute, call, memory, SSE-float, and block-chain. Run cached-only
and forced-emission samples on the same release WASM build, pair rounds, discard warm-up rounds,
verify output and retired counts, and report median guest instructions/second plus compilations,
executions, chain transitions, register-register hits, and fallbacks. Do not infer product-level
speed from these microfixtures or guest/HLE-only target traces.

## Verification strategy

1. Add failing Rust tests for complete `JitState` round-trip, TLB entry selection/invalidation, and
   each new instruction's translation and state effects.
2. Use the test-only WASM snapshot export to compare every CPU field and every mapped guest page
   after deterministic fixtures in interpreter, cached-only, and emitted runs.
3. Run focused CPU tests and WASM smoke/browser JIT checks after each slice. Preserve the current
   separate graphics failure as an independently reported result.
4. Run same-build paired benchmarks only after correctness succeeds; update `CLAUDE.md` and
   `docs/STATUS.md` with achieved results and remaining gaps.

## Limits

This work does not turn the interpreter into a complete x86-64 implementation, model x87 extended
precision, add AVX/EVEX coverage, or make generated code safe for host concurrency. Unsupported
instructions and runtime boundaries continue through established interpreter/HLE paths. The
benchmark fixtures measure guest instruction throughput in one environment and do not establish
game-level or cross-browser performance.
