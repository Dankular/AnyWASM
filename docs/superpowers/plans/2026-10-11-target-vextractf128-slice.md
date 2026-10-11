# VEXTRACTF128 Implementation Plan

> **For the implementer:** Add only the `VEXTRACTF128` form reached by the selected target.

**Goal:** Resume the selected GLFWDemo through its upper-lane callback extraction at RVA `0x36B3`.

**Design:** `docs/superpowers/specs/2026-10-11-target-vextractf128-slice.md` records the target
bytes, allowed encoding, semantics, and limits.

### Task 1: Add target decode and execution regressions

- [x] Check exact target decode, invalid forms, register and memory destinations, upper-lane result,
      YMM clearing, and flags preservation.

### Task 2: Implement the bounded instruction form

- [x] Decode only VEX3 `0F3A 19` with `66`, `L=1`, `W=0`, and reserved `vvvv`.
- [x] Execute the immediate-selected 128-bit lane into XMM or memory.

### Task 3: Verify target progress

- [x] Run the focused CPU decode/execution regressions.
- [x] Rerun the selected 30,000,000-instruction target; it advances to guest `ExitProcess(3)`.
- [x] Rerun final workspace and WASM smoke verification after this slice and update docs/issues.
