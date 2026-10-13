# Test fixture provenance

`diligent_glfwdemo_sdf.dxbc` is the 1,636-byte `GenerateSDF.hlsl` compute shader from
DiligentSamples commit `03a10e15e69c5ae1738a52175ae029f3f4275a90`, pinned by DiligentEngine commit
`efe5da2270c6edfc8b0d2e39e946b35acbeaa874`. Microsoft HLSL Shader Compiler 10.1 compiled it with
`RADIUS=8`, `DIST_SCALE=0.5`, and `DST_TEXTURE_FORMAT_32F=1`. The target shader is licensed under
Apache-2.0; the license text is included at
[`licenses/DiligentEngine-License.txt`](licenses/DiligentEngine-License.txt).

`jit_movzx_x86.exe` is a freestanding PE32 fixture for the adjacent
`MOVZX EBX, byte ptr [ESI]` / `MOVZX EAX, BL` sequence and the scaled `LEA` pair observed in GTA's
parser. Rebuild it with
`powershell.exe -NoProfile -ExecutionPolicy Bypass -File fixtures/build_jit_movzx_x86.ps1`; run
`node test/wasm_jit_x86_movzx.mjs` to compare interpreter, cached-only, and emitted WASM execution,
including complete CPU snapshots, mapped memory, and retired-instruction counts. The fixture reads
the byte once in the interpreter before entering the pair's block so its data-page TLB is warm when
the emitted load runs.
