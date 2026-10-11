# Target SHRX Slice Implementation Plan

> **For the implementer:** Execute the steps in order and keep unsupported variants fail-loud.

**Goal:** Decode and execute only the 32-bit SHRX forms directly reached in the selected GLFWDemo
target.

**Design:** `docs/superpowers/specs/2026-10-11-target-shrx-slice.md` records the exact target bytes,
Intel-defined operand order, count mask, flag behavior, and explicit scope boundary.

**Changes:** `crates/anywasm-cpu/src/decode.rs`, `crates/anywasm-cpu/src/exec.rs`, and focused
CPU regressions in `crates/anywasm-cpu/tests/vex_movups.rs`.

### Task 1: Add decoder regressions

- [x] Assert the exact target bytes decode as SHRX eax, ebp, eax with 32-bit operands and length 5.
- [x] Assert L=1, wrong mandatory prefix, and W=1 variants remain unsupported.
- [x] Run the focused test and confirm it fails on the current unsupported VEX3 path.

### Task 2: Add execution regressions

- [x] Cover register and memory value sources, 0/32 count masking, result zero-extension, and
      unchanged RFLAGS.
- [x] Run focused execution tests and confirm they fail before implementation.

### Task 3: Implement the narrow decoder and executor

- [x] Add `Op::Shrx` and recognize only the documented VEX.LZ.F2.0F38.W0 F7 encoding.
- [x] Execute logical right shift with a five-bit count mask without modifying RFLAGS.
- [x] Run focused SHRX tests and existing SHLX / ANDN VEX regressions.

### Task 4: Verify target progress

- [x] Run `cargo fmt --all` and `cargo test -p anywasm-cpu`.
- [x] Rerun the selected target with import tracing and the same 30,000,000-instruction budget.
- [x] Record the actual stop and keep renderer/game acceptance claims bounded by observed evidence.

## Reconciliation (2026-10-11)

Decoder and execution regressions, including register/memory operands and count masking, pass in the
workspace suite. The selected target's 30,000,000-instruction checkpoint is documented in
`docs/TARGETS.md`; it does not imply renderer or gameplay acceptance.