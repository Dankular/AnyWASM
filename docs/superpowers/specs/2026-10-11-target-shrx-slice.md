# Target-Grounded SHRX Slice

**Status:** narrow design for the next instruction reached by the selected Diligent target.

## Target evidence

After the Win64 scheduler, target-observed AVX2 slice, Win32 version verification, DXGI interface
version fix, and virtual frame-latency wait handle, the selected D3D11-only `/MT` `GLFWDemo.exe`
was run for up to 30,000,000 guest instructions. It stopped in `GLFWDemo.exe` at RIP
`0x14003ffa5` (RVA `0x3ffa5`) with `Decode(Unsupported("VEX3 0F38 instruction"))`. The bytes at
that RVA are:

```text
C4 E2 7B F7 C5    SHRX eax, ebp, eax
8B C8             mov ecx, eax
41 23 C1          and eax, r9d
F7 D1             not ecx
23 C6             and eax, esi
41 0B CA          or ecx, r10d
23 CA             and ecx, edx
0B C8             or ecx, eax
41 89 08          mov [r8], ecx
```

The function contains a second identical SHRX encoding at RVA `0x3ffef`. Intel's SDM identifies
`VEX.LZ.F2.0F38.W0 F7 /r` as 32-bit SHRX, with ModRM.reg as destination, ModRM.r/m as value, and
VEX.vvvv as count. The count is masked to five bits and flags are unchanged. See the official
[Intel SDM instruction reference](https://cdrdv2-public.intel.com/782151/253667-sdm-vol-2b.pdf).

## Goal and limits

Implement only the target-observed 32-bit SHRX register and memory-source forms. Preserve the
existing SHLX behavior and reject unsupported SHRX widths or prefix variants. Do not add SARX,
other BMI2 instructions, general VEX support, or unrelated instructions in this slice.

## Semantics

- `SHRX r32, r/m32, r32` shifts the source right logically by `count & 31` and writes the low
  32-bit result to the destination, zero-extending it to 64 bits.
- It leaves RFLAGS unchanged.
- Register and memory sources use existing operand helpers and memory fault behavior.
- The decoder accepts VEX.L=0, pp=F2, map=0F38, W=0 only; other forms stay unsupported.

## Verification

1. Decoder regressions assert the exact target bytes, operands, width, and length, and reject wrong
   VEX.L, mandatory prefix, and W variants.
2. Execution regressions cover register/memory source, masked counts (including 0 and 32),
   zero-extension, and unchanged RFLAGS.
3. Run the CPU tests and workspace suite, then rerun the same bounded target and record the next
   blocker and any actual window, renderer, frame, or gameplay evidence separately.

This instruction-level checkpoint is not game or renderer acceptance.
