# Target-Grounded VEXTRACTF128 Slice

**Status:** implemented and verified against one instruction observed in the selected Diligent
GLFW Demo trace.

## Target evidence

After static PE TLS initialization, the D3D11-only `/MT` GLFWDemo reaches
`GLFWDemo.exe+0x36B3`, where it stops on bytes `C4 E3 7D 19 C0 01`:

```text
vextractf128 xmm0, ymm0, 1
```

The next instructions move the low qword of `xmm0` to `rax` and call it, so the target is using the
upper 128-bit lane of `ymm0` as a callback address. The decoder already handles VEX `0F3A` forms
such as `VROUNDSS`, `VPEXTRQ`, and `VINSERTPS`, but not this instruction.

## Scope

Support VEX.256.66.0F3A.W0 opcode `19` with reserved `vvvv=1111b`, register or memory destination,
and the architectural immediate lane selector. A register destination receives the selected 128-bit
lane and has its YMM upper half cleared. A memory destination writes 16 bytes. Flags remain
unchanged. Other encodings remain unsupported.

This is a bounded CPU instruction slice. It does not imply general AVX/AVX2 support or Diligent
renderer acceptance.

## Verification

1. Decode the exact target bytes and reject invalid VEX length, prefix, and `vvvv` forms.
2. Verify upper-lane extraction to an XMM register, YMM upper clearing, memory stores, and unchanged
   flags.
3. Rerun the selected target with import tracing and the same 30,000,000-instruction budget; record
   the next actual stop.
