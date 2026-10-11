# Target BZHI Slice Implementation Plan

> **For the implementer:** Keep the decoder limited to the exact target-observed 32-bit form.

**Goal:** Add target-reached `BZHI r32, r/m32, r32`.

**Design:** `docs/superpowers/specs/2026-10-11-target-bzhi-slice.md` records exact bytes, operand
fields, semantics, flags, and scope.

**Changes:** `crates/anywasm-cpu/src/decode.rs`, `crates/anywasm-cpu/src/exec.rs`, and focused tests
in `crates/anywasm-cpu/tests/vex_movups.rs`.

### Task 1: Add decoder and execution regressions

- [x] Assert the exact target bytes decode with ModRM.reg destination, SIB memory source, and
      VEX.vvvv index register.
- [x] Test index 0, 4, 31, 32, and a value above 32, plus all documented flags and GPR zero-extension.
- [x] Confirm focused tests fail on the unsupported current decode path.

### Task 2: Implement target form

- [x] Add `Op::Bzhi`, decode only VEX.LZ.0F38.W0 F5, and use existing operand helpers.
- [x] Execute low-eight-bit index semantics, memory reads, and documented flag behavior.
- [x] Keep W1 and unrelated VEX BMI2 encodings unsupported.
- [x] Run focused tests and neighboring ANDN/SHLX/SHRX regressions.

### Task 3: Verify target progress

- [x] Run `cargo test -p anywasm-cpu`.
- [x] Rerun the selected target at 30,000,000 guest instructions with import tracing.
- [x] Record the actual stop and distinguish guest progress from real rendering/game acceptance.

## Reconciliation (2026-10-11)

Decoder/execution regressions pass in the workspace suite. Linux CI host-differential coverage checks
the target memory form at indexes 0, 1, 31, 32, 33, 0x101, and 255. The target trace and bounded
acceptance limits are recorded in `docs/TARGETS.md`.