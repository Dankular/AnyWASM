# Target-Grounded BZHI Slice

**Status:** bounded design for a 32-bit BMI2 instruction reached by the selected GLFWDemo target.

## Target evidence

After the target-specific VEX move/extract slice, the D3D11-only `/MT` `GLFWDemo.exe` stopped at
RIP `0x14004421c` (RVA `0x4421c`) with `Decode(Unsupported("VEX3 0F38 instruction"))`. The exact
instruction is:

```text
C4 C2 70 F5 44 91 FC    BZHI eax, dword ptr [r9+rdx*4-4], ecx
```

Intel's SDM specifies VEX.NDS.LZ.0F38.W0 `F5 /r` as `BZHI r32, r/m32, r32`. ModRM.reg is the
destination, ModRM.r/m the value, and VEX.vvvv the index. It uses the low eight index bits; an
index below 32 masks off bits at and above the index, while an index over 31 copies the source and
sets CF. ZF, CF, and SF are set from the result/index, OF is cleared, and AF/PF are undefined. See
the official [Intel SDM instruction reference](https://www.intel.cn/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developers-manual.pdf).

## Goal and limits

Implement only the target-observed 32-bit BZHI form, including register and memory sources. Do not
add the 64-bit form or other BMI1/BMI2 instructions in this slice. Unsupported VEX variants must
continue to fail-loud.

## Semantics

- Read the source and index as 32-bit operands; use `index & 0xff`.
- For index `< 32`, return `source & ((1 << index) - 1)`; for index `>= 32`, return source
  unchanged and set CF.
- Set ZF and SF from the 32-bit result; clear OF. Preserve AF/PF deterministically because Intel
  leaves those flags undefined for BZHI.
- A 32-bit destination write zero-extends to the full GPR.

## Verification

1. Decode the exact target bytes and assert destination, SIB memory source, VEX.vvvv index register,
   width, and instruction length. Reject VEX.L=1, W=1, and wrong map/prefix variants.
2. Execute count 0, count below 32, count 31, and count 32/over; assert result, documented flags,
   zero-extension, and memory addressing.
3. Run focused CPU tests and the package suite, then rerun the 30,000,000-instruction target and
   record its new checkpoint without claiming renderer or game acceptance.

This instruction slice is target evidence, not broad BMI2 or CPU acceptance.
