# Target-Grounded VEX Move and Extract Slice

**Status:** bounded design for two VEX forms found in the selected GLFWDemo executable.

## Target evidence

After the target-specific SHRX implementation, the D3D11-only `/MT` `GLFWDemo.exe` advanced to
RIP `0x1400440c6` (RVA `0x440c6`) and stopped at `Decode(Unsupported("VEX2 instruction"))`. The
first target instruction is:

```text
C4 C1 F9 7E C0    VMOVQ r8, xmm0
```

Later in the same function, at RVA `0x440d6`, the observed bytes are:

```text
C4 E3 F9 16 C0 01    VPEXTRQ rax, xmm0, 1
```

Intel's SDM defines the first as VEX.128.66.0F.W1 `7E /r`, moving a qword from XMM to r/m64,
and the second as VEX.128.66.0F3A.W1 `16 /r ib`, extracting the selected qword to r/m64. See the
official [Intel SDM instruction reference](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf).

## Goal and limits

Decode and execute only these 128-bit, 64-bit-destination forms reached in the target. Reuse the
existing scalar `MovdFromX` and `Pextr(8)` execution semantics. Keep other VMOVQ directions,
VEX.L=1, and unsupported VEX encodings fail-loud. This is not a general AVX or AVX2 expansion.

## Semantics

- `VMOVQ r/m64, xmm` copies XMM bits 63:0 to the 64-bit register or memory destination.
- `VPEXTRQ r/m64, xmm, imm8` writes XMM lane `(imm8 & 1)` to the 64-bit register or memory
  destination.
- Both instructions leave RFLAGS unchanged. Their target VEX encodings require VEX.vvvv=1111b
  and VEX.L=0.

## Verification

1. Decoder tests assert target bytes, operand order, width, and length. Wrong L, vvvv, and
   unsupported prefix/direction variants stay rejected.
2. Execution tests cover register and memory destinations, both extracted qword lanes, and
   unchanged RFLAGS.
3. Run focused CPU tests, the CPU package tests, then rerun the same 30,000,000-instruction target
   and document the next stop and observed rendering evidence accurately.

No window, real renderer, rendered frame, or gameplay acceptance is implied by decoding these forms.
