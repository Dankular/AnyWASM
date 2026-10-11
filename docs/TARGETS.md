# Windows target evaluation

These are analysis fixtures for measuring real Win64 program import coverage. They are not yet
runtime acceptance passes: `anywasm analyze` found unresolved imports for every executable, and none
has been run through AnyWASM or pixel-verified.

## Sources and build

Built from DiligentEngine `efe5da2270c6edfc8b0d2e39e946b35acbeaa874`, with DiligentCore at
`074060cfd1db73064d7a18cf40df86af6531520c` and DiligentSamples at
`03a10e15e69c5ae1738a52175ae029f3f4275a90`. The upstream DiligentSamples project declares
Apache-2.0; its repository also notes that dependencies can have independent licences. The
Asteroids source carries an Intel-origin notice, so its code and assets need separate redistribution
review before it can be an acceptance target. No sample binaries or assets are copied into AnyWASM.

The executables were built with Visual Studio 2022 / MSVC 19.44 and Windows SDK 10.0.26100, using
CMake's Visual Studio 17 2022 x64 generator. D3D11 and D3D12 were enabled; OpenGL, Vulkan and WebGPU
were disabled. The local external checkout needed CMake workarounds to locate Git and the SDK's
`fxc.exe` from generated build rules. Those edits are confined to `target-eval/DiligentEngine` and
are not part of this repository.

## Analyzer results

| Program | What it exercises | Analyzer result | Main blockers |
|---|---|---:|---|
| `Tutorial01_HelloTriangle.exe` | Win64 GUI triangle tutorial; built with D3D11 support and offers D3D11 at the device-selection dialog | 150 implemented, 100 missing | 65 MSVCP140 C++ runtime symbols, 16 USER32 symbols, 4 IMM32 symbols, 6 VCRUNTIME140 symbols, plus CRT and shell imports |
| `Tutorial03_Texturing.exe` | Win64 GUI textured cube tutorial; loads `DGLogo.png`; built with D3D11 support and offers D3D11 at the device-selection dialog | 151 implemented, 100 missing | Same C++ runtime and Win32 GUI/IME gaps as HelloTriangle, plus `ShellExecuteW` |
| `Asteroids.exe` | Win64 50,000-asteroid benchmark; includes native and Diligent D3D11 and D3D12 modes | 136 implemented, 116 missing | 68 MSVCP140, 17 CONCRT140, 9 NInput, 6 VCRUNTIME140 symbols; D3D12 serialization and several kernel32/user32 imports |

The tutorial sources are in the [DiligentSamples repository](https://github.com/DiligentGraphics/DiligentSamples),
which declares Apache-2.0 for the project while listing third-party dependencies separately. See the
[Tutorial 03 source](https://github.com/DiligentGraphics/DiligentSamples/tree/03a10e15e69c5ae1738a52175ae029f3f4275a90/Tutorials/Tutorial03_Texturing)
and the [Asteroids README](https://github.com/DiligentGraphics/DiligentSamples/blob/03a10e15e69c5ae1738a52175ae029f3f4275a90/Samples/Asteroids/README.md)
for the source programs and their stated rendering modes.

`analyze` reports statically imported symbols. The Diligent tutorials present a device-selection
dialog; these binaries were analyzed, but the D3D11 path was not launched. A missing symbol is a blocker if the guest calls it;
the report does not prove every import is reached on a selected rendering path. The imported counts
therefore prioritize implementation work, but do not replace a runtime trace. In particular, the
two tutorials are sample applications rather than games, and Asteroids remains only a candidate
until its redistribution provenance is resolved and its imports are supported well enough to run.

## Next evidence needed

- Trace each executable through `anywasm run` after the top-level CRT, C++ runtime, USER32 and IMM32
  dependencies are available; record the first runtime blocker and compare it with `analyze`.
- Keep Asteroids outside the redistributable acceptance set until the Intel-origin code and sample
  media have a documented licence review.
- Replace or supplement the tutorials with a redistributable Win64 game once one can be built and
  analyzed without importing a second graphics API that obscures D3D11-specific gaps.
