# Target-Grounded AVX2 Slice

**Status:** bounded design for the first AVX2 instructions reached by the selected Diligent target.

## Target evidence

The selected D3D11-only `/MT` `GLFWDemo.exe` and `GraphicsEngineD3D11_64r.dll` were rerun with
30,000,000 guest instructions after cooperative scheduling was added. The runtime progressed past
`CreateWindowExW`, `ShowWindow`, `D3D11CreateDevice`, and a guest `CreateThread` call, then stopped
at RIP `0x1800598d4` in `GraphicsEngineD3D11_64r.dll` with
`Decode(Unsupported("VEX2 instruction"))`. The DLL has preferred base `0x180000000`; at RVA
`0x598d4`, the bytes are:

```text
C5 F9 6E C0          VMOVD xmm0, eax
C4 E2 7D 58 C0       VPBROADCASTD ymm0, xmm0
C5 FD FE CA          VPADDD ymm1, ymm0, ymm2
C4 C1 7E 7F 49 C0    VMOVDQU [r9-0x40], ymm1
```

Only the first instruction is the current stop. The following bytes are a static decode of the same
target block and must be verified as execution advances. Intel's instruction reference specifies
VEX.128.66.0F.W0 `6E /r` as `VMOVD xmm, r32/m32`, VEX.256.66.0F38.W0 `58 /r` as `VPBROADCASTD`,
and VEX.256.66.0F.WIG `FE /r` as `VPADDD` (Intel SDM and Instruction Set Extensions reference,
[`VMOVD`](https://www.intel.com/content/dam/develop/external/us/en/documents/319433-024-697869.pdf),
[`VPBROADCASTD`](https://cdrdv2-public.intel.com/789581/325383-sdm-vol-2abcd.pdf),
[`VPADDD`](https://cdrdv2-public.intel.com/671200/325462-sdm-vol-1-2abcd-3abcd.pdf)).

No visible window, actual host D3D11 device, rendered frame, or gameplay behavior has been observed.

## Goal and limits

Implement and verify only the target-observed VMOVD, 256-bit VPBROADCASTD, and 256-bit VPADDD
forms. Confirm the existing 256-bit VMOVDQU store decodes and preserves the target's YMM result.
Do not add general AVX2, EVEX, FMA, gather, or unrelated vector operations in this slice. Every
other unsupported encoding must continue to fail with its exact decode error.

## Semantics

- `VMOVD xmm1, r32/m32` copies the source dword to bits 31:0 of the destination, clears the rest of
  XMM1, and clears YMM1 bits 255:128 as required by a VEX.128 write.
- `VPBROADCASTD ymm1, xmm2/m32` copies source bits 31:0 into all eight 32-bit lanes of YMM1.
- `VPADDD ymm1, ymm2, ymm3/m256` adds eight pairs of 32-bit lanes modulo `2^32`.
- These integer instructions leave RFLAGS unchanged. Memory operands use the existing checked guest
  memory read/write path, and vector register operands use the full YMM snapshot.
- The existing `VMOVDQU` 256-bit store form is part of the checkpoint only if the test proves it
  stores all 32 bytes and leaves the source vector unchanged.

## Verification

1. Decoder tests use the exact byte encodings above and assert instruction kind, operands, width,
   and instruction length. Invalid prefix/map/width variants remain unsupported.
2. Execution tests cover GPR and memory VMOVD sources, XMM and memory broadcast sources, eight-lane
   wrapping VPADDD, YMM upper lanes, unchanged RFLAGS, and the 32-byte VMOVDQU store.
3. Run the relevant CPU tests and the full workspace suite; run the hardware differential cases when
   the host supports the required AVX2 instructions.
4. Rerun the same bounded target with import tracing. Record the next blocker and whether any window,
   real renderer, frame, or game behavior was actually observed.

The target trace is evidence for the chosen forms, not acceptance of the game. AVX2 support should
remain listed as partial after this slice.
