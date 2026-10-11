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

Reports below were regenerated with `anywasm analyze <exe> --json` at AnyWASM commit
`58e32cee66e3203667ea98bc97dc62b7a12069f3`. The binaries were read from the external build tree;
none were copied into this repository:

- `D:\target-eval\DiligentBuild\DiligentSamples\Tutorials\Tutorial01_HelloTriangle\Release\Tutorial01_HelloTriangle.exe`
- `D:\target-eval\DiligentBuild\DiligentSamples\Tutorials\Tutorial03_Texturing\Release\Tutorial03_Texturing.exe`
- `D:\target-eval\DiligentBuild\DiligentSamples\Samples\Asteroids\Release\Asteroids.exe`

The source-base revisions for these builds are pinned and verified: DiligentEngine
`efe5da2270c6edfc8b0d2e39e946b35acbeaa874`, DiligentCore
`074060cfd1db73064d7a18cf40df86af6531520c`, and DiligentSamples
`03a10e15e69c5ae1738a52175ae029f3f4275a90`.

| Program | What it exercises | Analyzer result | Missing blocker counts |
|---|---|---:|---|
| `Tutorial01_HelloTriangle.exe` | Win64 GUI triangle tutorial; built with D3D11 support and offers D3D11 at the device-selection dialog | 152 implemented, 98 missing | MSVCP140 65; USER32 15; IMM32 4; VCRUNTIME140 6; KERNEL32 1; SHELL32 1; UCRT API sets 6 |
| `Tutorial03_Texturing.exe` | Win64 GUI textured cube tutorial; loads `DGLogo.png`; built with D3D11 support and offers D3D11 at the device-selection dialog | 153 implemented, 98 missing | MSVCP140 65; USER32 15; IMM32 4; VCRUNTIME140 6; KERNEL32 1; SHELL32 1; UCRT API sets 6 |
| `Asteroids.exe` | Win64 50,000-asteroid benchmark; includes native and Diligent D3D11 and D3D12 modes | 137 implemented, 115 missing | MSVCP140 68; CONCRT140 17; NInput 9; VCRUNTIME140 6; D3D12 2; USER32 4; KERNEL32 1; UCRT API sets 8 |

### Exact missing imports

These are the missing names reported by the analyzer. A dash means no missing import for that DLL;
the D3D12 and NInput DLLs do not appear in either tutorial's import report.

| DLL | Tutorial01_HelloTriangle | Tutorial03_Texturing | Asteroids |
|---|---|---|---|
| `KERNEL32.dll` | `SleepConditionVariableSRW` | `SleepConditionVariableSRW` | `SleepConditionVariableSRW` |
| `USER32.dll` | `TrackMouseEvent`, `GetKeyboardLayout`, `IsWindowUnicode`, `GetDesktopWindow`, `EndDialog`, `LoadBitmapW`, `GetDlgItem`, `EnumDisplaySettingsW`, `DialogBoxParamW`, `GetMessageExtraInfo`, `SetClipboardData`, `GetClipboardData`, `EmptyClipboard`, `CloseClipboard`, `OpenClipboard` | Same 15 symbols as Tutorial01_HelloTriangle | `GetWindowPlacement`, `EnableMouseInPointer`, `SetWindowPlacement`, `GetPointerInfo` |
| `IMM32.dll` | `ImmSetCandidateWindow`, `ImmReleaseContext`, `ImmGetContext`, `ImmSetCompositionWindow` | Same 4 symbols as Tutorial01_HelloTriangle | — |
| `SHELL32.dll` | `ShellExecuteW` | `ShellExecuteW` | — |
| `d3d12.dll` | — | — | `D3D12SerializeRootSignature`, ordinal `#101` |
| `NInput.dll` | — | — | `SetPropertyInteractionContext`, `RemovePointerInteractionContext`, `AddPointerInteractionContext`, `ProcessInertiaInteractionContext`, `DestroyInteractionContext`, `ProcessPointerFramesInteractionContext`, `RegisterOutputCallbackInteractionContext`, `CreateInteractionContext`, `SetInteractionConfigurationInteractionContext` |
| `api-ms-win-crt-heap-l1-1-0.dll` | `_callnewh` | `_callnewh` | `_callnewh` |
| `api-ms-win-crt-math-l1-1-0.dll` | — | — | `ilogbf`, `scalbnf` |
| `api-ms-win-crt-runtime-l1-1-0.dll` | `_register_thread_local_exe_atexit_callback`, `strerror_s`, `_invoke_watson`, `terminate` | `_register_thread_local_exe_atexit_callback`, `strerror_s`, `_invoke_watson`, `terminate` | `_register_thread_local_exe_atexit_callback`, `strerror_s`, `_invoke_watson`, `terminate` |
| `api-ms-win-crt-stdio-l1-1-0.dll` | `_wfopen_s` | `_wfopen_s` | `_wfopen_s` |
| `VCRUNTIME140.dll` | `__intrinsic_setjmp`, `_purecall`, `__std_exception_destroy`, `__std_exception_copy`, `__current_exception`, `__current_exception_context` | Same 6 symbols as Tutorial01_HelloTriangle | Same 6 symbols as Tutorial01_HelloTriangle |

All three reports also import `VCRUNTIME140_1.dll`; its single imported symbol is implemented in
each. Other imported `api-ms-win-crt-*` symbols are implemented.

| Runtime group | Tutorial01_HelloTriangle | Tutorial03_Texturing | Asteroids | Aggregate missing |
|---|---:|---:|---:|---:|
| `MSVCP140.dll` (C++ standard library) | 65 | 65 | 68 | 198 |
| `VCRUNTIME140.dll` | 6 | 6 | 6 | 18 |
| MSVCP140 + VCRUNTIME140 | 71 | 71 | 74 | 216 |
| `CONCRT140.dll` | 0 | 0 | 17 | 17 |

The tutorial sources are in the [DiligentSamples repository](https://github.com/DiligentGraphics/DiligentSamples),
which declares Apache-2.0 for the project while listing third-party dependencies separately. See the
[Tutorial 03 source](https://github.com/DiligentGraphics/DiligentSamples/tree/03a10e15e69c5ae1738a52175ae029f3f4275a90/Tutorials/Tutorial03_Texturing)
and the [Asteroids README](https://github.com/DiligentGraphics/DiligentSamples/blob/03a10e15e69c5ae1738a52175ae029f3f4275a90/Samples/Asteroids/README.md)
for the source programs and their stated rendering modes.

`analyze` reports statically imported symbols. The Diligent tutorials present a device-selection
dialog; these binaries were analyzed, but the D3D11 path was not launched. Asteroids was analyzed
only; it was not run through AnyWASM. A missing symbol is a blocker if the guest calls it; the
report does not prove every import is reached on a selected rendering path. These counts prioritize
implementation work but do not replace a runtime trace. The tutorials are samples, not games, and
all three programs remain preliminary candidates, not acceptance runs. Asteroids also remains
outside the redistributable acceptance set until its Intel-origin code and sample media have a
documented licence review.

## Next evidence needed

- Trace each executable on its D3D11 path and record the first runtime blocker, distinguishing
  imports reached on that path from static-only imports. `GetCapture` is now implemented; each
  tutorial still reports 15 missing USER32 symbols and 4 missing IMM32 symbols.
- Keep Asteroids outside the redistributable acceptance set until the Intel-origin code and sample
  media have a documented licence review.
- Replace or supplement the tutorials with a redistributable Win64 game once one can be built and
  analyzed without importing a second graphics API that obscures D3D11-specific gaps.
