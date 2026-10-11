# Target VEX Move and Extract Implementation Plan

> **For the implementer:** Execute the tasks in order and keep unsupported forms fail-loud.

**Goal:** Add the target-observed VEX `VMOVQ r/m64, xmm` and `VPEXTRQ r/m64, xmm, imm8` forms.

**Design:** `docs/superpowers/specs/2026-10-11-target-vex-movq-extract-slice.md` records exact
bytes, target locations, Intel-defined semantics, and scope.

**Changes:** `crates/anywasm-cpu/src/decode.rs` and focused decoder/executor cases in
`crates/anywasm-cpu/tests/vex_movups.rs`; reuse existing execution ops in `sse.rs`.

### Task 1: Add decoder tests

- [x] Assert `C4 C1 F9 7E C0` decodes as `MovdFromX` from XMM0 to R8 with a 64-bit width.
- [x] Assert `C4 E3 F9 16 C0 01` decodes as `Pextr(8)` to RAX from XMM0, immediate 1.
- [x] Reject unsupported L, vvvv, and VMOVQ direction variants.
- [x] Run the new test and confirm the current decoder rejects the target VMOVQ form.

### Task 2: Add execution tests

- [x] Cover VMOVQ register and memory destinations and verify only the source low qword is copied.
- [x] Cover VPEXTRQ lanes 0 and 1, register and memory destinations, and unchanged RFLAGS.
- [x] Run the focused tests and confirm the exact target forms are initially unsupported.

### Task 3: Implement exact decoder paths

- [x] Map VEX.128.66.0F.W1 `7E /r` to existing `MovdFromX` with a 64-bit operand.
- [x] Map VEX.128.66.0F3A.W1 `16 /r ib` to existing `Pextr(8)`.
- [x] Require target reserved VEX.vvvv bits and VEX.L=0; preserve errors for other encodings.
- [x] Run new tests and existing VEX movement and PEXTR regressions.

### Task 4: Verify target progress

- [x] Run `cargo test -p anywasm-cpu`.
- [x] Rerun the selected target with the same 30,000,000-instruction budget and trace imports.
- [x] Record actual progress, the next stop, and any window or rendering evidence separately.

## Reconciliation (2026-10-11)

The exact decoder/execution regressions for VMOVQ and VPEXTRQ pass in the workspace suite. The
bounded selected-target trace and its rendering limits are recorded in `docs/TARGETS.md`.