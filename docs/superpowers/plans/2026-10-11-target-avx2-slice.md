# Target-Grounded AVX2 Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Decode and execute the exact VMOVD, 256-bit VPBROADCASTD, and 256-bit VPADDD forms reached by the selected Diligent D3D11 target.

**Architecture:** Extend the existing VEX decoder and interpreter with explicit operations for the observed forms. Keep vector writes in the current XMM/YMM CPU state and use the existing guest-memory helpers for scalar and 256-bit memory operands.

**Tech Stack:** Rust x86-64 decoder/interpreter, existing CPU unit and differential tests, selected Win64 PE target.

**Spec:** `docs/superpowers/specs/2026-10-11-target-avx2-slice.md`

## Global Constraints

- Support only the exact forms and widths listed in the spec.
- Preserve fail-loudly decode behavior for every unsupported prefix, map, width, and opcode.
- Leave flags unchanged for these integer/vector move operations.
- A successful bounded target run does not imply a rendered frame or gameplay acceptance.

## Review Focus

- VEX.128 VMOVD clears both the remaining XMM bits and the upper YMM half.
- VPBROADCASTD reads only a 32-bit memory source or the low dword of an XMM source.
- VPADDD wraps each 32-bit lane independently and uses all eight YMM lanes.
- Existing VMOVDQU store writes exactly 32 bytes without mutating its source.
- Unsupported VEX variants still stop instead of falling through to a legacy instruction.

---

### Task 1: VEX VMOVD

**Files:** `crates/anywasm-cpu/src/decode.rs`, `crates/anywasm-cpu/src/exec.rs` (or the operation's existing execution module), CPU decoder/execution tests.

- [ ] Add an exact-byte decode test for `C5 F9 6E C0` and a memory-source variant; assert `VMOVD xmm0, eax/m32`, 128-bit width, and four-byte length.
- [ ] Run the focused test and confirm it fails because the VEX form is unsupported.
- [ ] Add a distinct VEX VMOVD operation or an equivalent explicit VEX marker so the interpreter clears the full destination XMM register and upper YMM half.
- [ ] Test low-dword copy, zeroed upper bits, unchanged RFLAGS, and memory faults; verify unsupported W/L/pp variants still fail.
- [ ] Run the focused CPU tests and record the passing result.

### Task 2: 256-bit integer broadcast and add

**Files:** `crates/anywasm-cpu/src/decode.rs`, `crates/anywasm-cpu/src/exec.rs` (or the operation's existing execution module), CPU decoder/execution tests.

- [ ] Add exact-byte decode tests for `C4 E2 7D 58 C0` and `C5 FD FE CA`; assert eight-lane YMM operands and instruction lengths.
- [ ] Run the focused tests and confirm both target forms fail before implementation.
- [ ] Implement only VEX.256.66.0F38.W0 opcode `58` VPBROADCASTD with XMM and scalar m32 sources, and VEX.256.66.0F.WIG opcode `FE` VPADDD with YMM/YMM-or-m256 operands.
- [ ] Test all eight broadcast lanes, scalar memory width, eight independent wrapping sums, unchanged RFLAGS, and unsupported width/prefix variants.
- [ ] Test the exact `C4 C1 7E 7F 49 C0` VMOVDQU store for 32 written bytes and unchanged YMM source; extend its implementation only if that test exposes a gap.
- [ ] Run focused CPU tests and hardware differential cases where supported.

### Task 3: Target checkpoint and documentation

**Files:** `docs/TARGETS.md`, `docs/STATUS.md`, `CLAUDE.md`.

- [ ] Run `cargo test --workspace` and `node test/wasm_smoke.mjs` and record the complete results.
- [ ] Rerun the selected `/MT` D3D11 target with a 30,000,000-instruction cap and import tracing; record the next exact stop.
- [ ] Update the AVX and scheduler checklist entries to match only tested behavior and target evidence; preserve the no-renderer/no-game-acceptance boundary.
- [ ] Commit the verified task slice.
