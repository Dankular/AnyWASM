# Target SHRX Slice Implementation Plan

> **For the implementer:** Execute the steps in order and keep unsupported variants fail-loud.

**Goal:** Decode and execute only the 32-bit SHRX forms directly reached in the selected GLFWDemo
target.

**Design:** `docs/superpowers/specs/2026-10-11-target-shrx-slice.md` records the exact target bytes,
Intel-defined operand order, count mask, flag behavior, and explicit scope boundary.

**Changes:** `crates/anywasm-cpu/src/decode.rs`, `crates/anywasm-cpu/src/exec.rs`, and focused
CPU regressions in `crates/anywasm-cpu/tests/vex_movups.rs`.

### Task 1: Add decoder regressions

- [ ] Assert the exact target bytes decode as SHRX eax, ebp, eax with 32-bit operands and length 5.
- [ ] Assert L=1, wrong mandatory prefix, and W=1 variants remain unsupported.
- [ ] Run the focused test and confirm it fails on the current unsupported VEX3 path.

### Task 2: Add execution regressions

- [ ] Cover register and memory value sources, 0/32 count masking, result zero-extension, and
      unchanged RFLAGS.
- [ ] Run focused execution tests and confirm they fail before implementation.

### Task 3: Implement the narrow decoder and executor

- [ ] Add `Op::Shrx` and recognize only the documented VEX.LZ.F2.0F38.W0 F7 encoding.
- [ ] Execute logical right shift with a five-bit count mask without modifying RFLAGS.
- [ ] Run focused SHRX tests and existing SHLX / ANDN VEX regressions.

### Task 4: Verify target progress

- [ ] Run `cargo fmt --all` and `cargo test -p anywasm-cpu`.
- [ ] Rerun the selected target with import tracing and the same 30,000,000-instruction budget.
- [ ] Record the actual stop and keep renderer/game acceptance claims bounded by observed evidence.
