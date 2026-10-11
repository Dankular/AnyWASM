# Windows target evaluation

These are analysis fixtures for measuring real Win64 program import coverage. `anywasm analyze`
found unresolved imports for every executable. Both D3D11 tutorials now have first-stop runtime
traces through AnyWASM, but neither reached shader creation, Present, or pixel verification;
Asteroids has not been run. None is a runtime acceptance pass.

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
`54bc2a05aacf722810ad7e6c8556a9eab8599023`. The binaries were read from the external build tree;
none were copied into this repository. The static-runtime report records the same analyzer revision.

- `D:\target-eval\DiligentBuild\DiligentSamples\Tutorials\Tutorial01_HelloTriangle\Release\Tutorial01_HelloTriangle.exe`
- `D:\target-eval\DiligentBuild\DiligentSamples\Tutorials\Tutorial03_Texturing\Release\Tutorial03_Texturing.exe`
- `D:\target-eval\DiligentBuild\DiligentSamples\Samples\Asteroids\Release\Asteroids.exe`

The source-base revisions for these builds are pinned and verified: DiligentEngine
`efe5da2270c6edfc8b0d2e39e946b35acbeaa874`, DiligentCore
`074060cfd1db73064d7a18cf40df86af6531520c`, and DiligentSamples
`03a10e15e69c5ae1738a52175ae029f3f4275a90`.

| Program | What it exercises | Analyzer result | Missing blocker counts |
|---|---|---:|---|
| `Tutorial01_HelloTriangle.exe` | Win64 GUI triangle tutorial; built with D3D11 support and offers D3D11 at the device-selection dialog | 157 implemented, 93 missing | MSVCP140 65; USER32 12; IMM32 4; VCRUNTIME140 6; SHELL32 1; UCRT API sets 5 |
| `Tutorial03_Texturing.exe` | Win64 GUI textured cube tutorial; loads `DGLogo.png`; built with D3D11 support and offers D3D11 at the device-selection dialog | 158 implemented, 93 missing | MSVCP140 65; USER32 12; IMM32 4; VCRUNTIME140 6; SHELL32 1; UCRT API sets 5 |
| `Asteroids.exe` | Win64 50,000-asteroid benchmark; includes native and Diligent D3D11 and D3D12 modes | 141 implemented, 111 missing | MSVCP140 68; CONCRT140 17; NInput 9; VCRUNTIME140 6; D3D12 2; USER32 2; UCRT API sets 7 |

### Exact missing imports

These are the missing names reported by the analyzer. A dash means no missing import for that DLL;
the D3D12 and NInput DLLs do not appear in either tutorial's import report.

| DLL | Tutorial01_HelloTriangle | Tutorial03_Texturing | Asteroids |
|---|---|---|---|
| `USER32.dll` | `TrackMouseEvent`, `GetKeyboardLayout`, `GetDesktopWindow`, `EndDialog`, `LoadBitmapW`, `DialogBoxParamW`, `GetMessageExtraInfo`, `SetClipboardData`, `GetClipboardData`, `EmptyClipboard`, `CloseClipboard`, `OpenClipboard` | Same 12 symbols as Tutorial01_HelloTriangle | `EnableMouseInPointer`, `GetPointerInfo` |
| `IMM32.dll` | `ImmSetCandidateWindow`, `ImmReleaseContext`, `ImmGetContext`, `ImmSetCompositionWindow` | Same 4 symbols as Tutorial01_HelloTriangle | — |
| `SHELL32.dll` | `ShellExecuteW` | `ShellExecuteW` | — |
| `d3d12.dll` | — | — | `D3D12SerializeRootSignature`, ordinal `#101` |
| `NInput.dll` | — | — | `SetPropertyInteractionContext`, `RemovePointerInteractionContext`, `AddPointerInteractionContext`, `ProcessInertiaInteractionContext`, `DestroyInteractionContext`, `ProcessPointerFramesInteractionContext`, `RegisterOutputCallbackInteractionContext`, `CreateInteractionContext`, `SetInteractionConfigurationInteractionContext` |
| `api-ms-win-crt-heap-l1-1-0.dll` | `_callnewh` | `_callnewh` | `_callnewh` |
| `api-ms-win-crt-math-l1-1-0.dll` | — | — | `ilogbf`, `scalbnf` |
| `api-ms-win-crt-runtime-l1-1-0.dll` | `_register_thread_local_exe_atexit_callback`, `strerror_s`, `_invoke_watson`, `terminate` | `_register_thread_local_exe_atexit_callback`, `strerror_s`, `_invoke_watson`, `terminate` | `_register_thread_local_exe_atexit_callback`, `strerror_s`, `_invoke_watson`, `terminate` |
| `VCRUNTIME140.dll` | `__intrinsic_setjmp`, `_purecall`, `__std_exception_destroy`, `__std_exception_copy`, `__current_exception`, `__current_exception_context` | Same 6 symbols as Tutorial01_HelloTriangle | Same 6 symbols as Tutorial01_HelloTriangle |

All three reports also import `VCRUNTIME140_1.dll`; its single imported symbol is implemented in
each. Other imported `api-ms-win-crt-*` symbols are implemented.

`SleepConditionVariableSRW` is now recognized by the HLE and therefore is not a static missing
import. At runtime, its infinite wait stops explicitly because no guest-thread scheduler can resume
the waiting thread; this is the current GLFW Demo trace blocker below.

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

`analyze` reports statically imported symbols. The Diligent tutorials offer a device-selection
dialog, so the runtime traces passed `--mode d3d11`; both reached virtual D3D11 device discovery and
creation before stopping at the first unresolved MSVC C++ runtime import. Asteroids was analyzed
only; it was not run through AnyWASM. A missing symbol is a blocker if the guest calls it; the
report does not prove every import is reached on a selected rendering path. These counts prioritize
implementation work but do not replace runtime traces. The tutorials are samples, not games, and all
three programs remain preliminary candidates, not acceptance runs. Asteroids also remains outside
the redistributable acceptance set until its Intel-origin code and sample media have a documented
licence review.

### First D3D11 tutorial runtime stops

The dynamic-runtime Release outputs were run with the selected D3D11 backend and a 500,000,000-step
cap. Both stopped before reaching the cap. Their traces pass adapter enumeration, `D3D11CreateDevice`,
and `ID3D11Device1::CheckFeatureSupport`, then stop at the same imported MSVCP140 constructor:
`??0?$basic_ios@DU?$char_traits@D@std@@@std@@IEAA@XZ`. The HLE trace labels the missing thunk
`unknown!` because that import is not implemented; the analyzer maps it to `MSVCP140.dll`.

| Program | AnyWASM stop | Import caller | Last reached D3D11 path |
|---|---|---:|---|
| `Tutorial01_HelloTriangle.exe` | exit 120, unimplemented `basic_ios<char>` constructor | `0x140003320` | `D3D11CreateDevice`, feature queries, adapter/output enumeration |
| `Tutorial03_Texturing.exe` | exit 120, unimplemented `basic_ios<char>` constructor | `0x140004660` | `D3D11CreateDevice`, feature queries, adapter/output enumeration |

Reproduction from the repository root (adjust `$exe` for each tutorial):

```powershell
$exe = 'D:\target-eval\DiligentBuild\DiligentSamples\Tutorials\Tutorial01_HelloTriangle\Release\Tutorial01_HelloTriangle.exe'
$dir = Split-Path -Parent $exe
.\target\debug\anywasm.exe run $exe --dir $dir --trace-imports --max-steps 500000000 -- --mode d3d11
```

The CLI now passes its full remaining step budget to each `Process::run` call and charges actual
retired instructions across frame stops. A regression fixture exercises a guest monitor callback
that exceeds the former 2,000,000-instruction internal slice. This removes an artificial CLI stop;
it does not make callbacks resumable when the configured run budget itself expires. These tutorial
traces did not reach shader compilation, guest `Present`, browser WebGPU execution, or visible pixels.

## Selected game candidate: DiligentSamples GLFW Demo

The pinned DiligentSamples README describes GLFW Demo as a simple maze mini-game. The repository
declares Apache-2.0 and says its contributions are free of IP encumbrances; it separately identifies
GLFW as zlib-licensed. The demo builds GLFW statically (there is no `glfw3.dll` output), uses
`GLFW_NO_API` for window/input, obtains the Win32 handle with `glfwGetWin32Window`, and loads the
D3D11 backend through `LoadAndGetEngineFactoryD3D11`. Its maze is generated procedurally; CMake
copies only `DrawMap.hlsl`, `GenerateSDF.hlsl`, `Structures.fxh`, and `RenderStates.json` to the
target output. The `sdf_map.jpg` source image is not copied there. Microsoft documents
`D3DCompiler_47.dll` as an application-local game redistributable. These source, asset, GLFW, and
compiler-runtime terms make this the selected candidate for evaluation. It was chosen over
Asteroids because upstream describes it as a mini-game, while Asteroids is an Intel-origin
performance benchmark with a separate source/media review still open. The built files remain in
the external temporary build tree and are not redistributed by AnyWASM. The output also imports the
MSVC C++ runtime; Microsoft states that redistributing those runtime files is subject to the
Visual Studio licence terms for a licensed Visual Studio user, so any later product bundle must
follow those terms.

Sources are pinned to DiligentEngine `efe5da2270c6edfc8b0d2e39e946b35acbeaa874`, DiligentCore
`074060cfd1db73064d7a18cf40df86af6531520c`, and DiligentSamples
`03a10e15e69c5ae1738a52175ae029f3f4275a90`. The sample README documents `--mode d3d11`; CMake
reported D3D11 as the only supported backend with D3D12, OpenGL, and Vulkan disabled. It was built
with Visual Studio 2022 x64 / Release in a temporary build directory outside this repository. The
external checkout has the previously documented local CMake workarounds. The configure used a
disconnected FetchContent mode and an empty NVAPI source override to avoid fetching an optional
NVIDIA dependency. It enabled samples/tools and kept the archiver and render-state packager enabled
because another generated sample target requires them at configure time.

```powershell
cmake -S '<external DiligentEngine checkout>' -B '<temporary build directory>' `
  -G 'Visual Studio 17 2022' -A x64 `
  -DDILIGENT_NO_DIRECT3D12=ON -DDILIGENT_NO_OPENGL=ON -DDILIGENT_NO_VULKAN=ON `
  -DDILIGENT_BUILD_SAMPLES=ON -DDILIGENT_BUILD_TOOLS=ON -DDILIGENT_BUILD_FX=OFF `
  -DDILIGENT_NO_SUPER_RESOLUTION=ON -DDILIGENT_NO_ARCHIVER=OFF `
  -DDILIGENT_NO_RENDER_STATE_PACKAGER=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON `
  -DFETCHCONTENT_SOURCE_DIR_NVAPI='<empty local directory>'
cmake --build '<temporary build directory>' --config Release --target GLFWDemo --parallel 4
```

| Output beside `GLFWDemo.exe` | Size (bytes) | Role |
|---|---:|---|
| `GLFWDemo.exe` | 656,384 | Win64 game executable |
| `GraphicsEngineD3D11_64r.dll` | 1,011,200 | Diligent D3D11 backend |
| `Archiver_64r.dll` | 785,920 | Copied Diligent support DLL; runtime use not established |
| `D3Dcompiler_47.dll` | 4,740,688 | HLSL compiler dependency |
| HLSL/FXH/JSON files | 11,999 total | Runtime shaders and render-state description |

`anywasm analyze --json` was run separately on the executable and all three DLLs using AnyWASM
`4b37b09` (the repository revision before this documentation update). These are static import counts;
they do not show which imports are reached after D3D11 selection. The complete missing-symbol lists
are preserved in [`target-imports/glfwdemo-d3d11.json`](target-imports/glfwdemo-d3d11.json).

| Image | Implemented | Missing | Largest unresolved groups |
|---|---:|---:|---|
| `GLFWDemo.exe` | 153 | 134 | MSVCP140 64; USER32 36; GDI32 13; VCRUNTIME140 5 |
| `GraphicsEngineD3D11_64r.dll` | 74 | 96 | MSVCP140 75; VCRUNTIME140 7; D3DCOMPILER_47 2; USER32 1; SHLWAPI 1 |
| `Archiver_64r.dll` | 68 | 91 | MSVCP140 73; VCRUNTIME140 7; D3DCOMPILER_47 2; SHLWAPI 1 |
| `D3Dcompiler_47.dll` | 103 | 28 | KERNEL32 15; ADVAPI32 12; RPCRT4 1 |

Notable unresolved names include `RegisterRawInputDevices`, `GetRawInputData`, `SetPixelFormat`,
`SwapBuffers`, and `D3DCompile`/`D3DReflect`, as well as large MSVC C++ runtime import sets. The
executable has 134 statically unresolved imports and the selected D3D11 engine DLL has 96. The
AnyWASM runtime's reached-import behavior was not attempted because this static unresolved-import
surface is large. This candidate has not been run or behavior-verified and does not pass the
real-target acceptance gate. A missing static import alone does not prove that a selected path
reaches it.

### Static MSVC runtime experiment

A separate D3D11-only build was configured under
`D:\Dev Proj\WASM\target-eval\DiligentBuild-GLFWDemo-staticcrt`; the original build tree and
external source checkout were preserved. The top-level project requires CMake 3.10, so the
configure set `CMAKE_POLICY_DEFAULT_CMP0091=NEW` along with
`CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded`. Generated MSBuild projects for `GLFWDemo`, the D3D11
engine, Common, and GLFW all specify `RuntimeLibrary=MultiThreaded`. DiligentFX was enabled because
this DiligentSamples checkout requires it; the already-populated EnTT and Abseil source directories
from the prior external build were passed as disconnected FetchContent overrides. CMake reported
D3D11 as the only enabled graphics backend, and the Release `GLFWDemo` target built successfully.
No binary was copied into this repository.

The current static-runtime analyzer output is in
[`target-imports/glfwdemo-d3d11-staticcrt.json`](target-imports/glfwdemo-d3d11-staticcrt.json).
The earlier dynamic-runtime report remains in
[`target-imports/glfwdemo-d3d11.json`](target-imports/glfwdemo-d3d11.json), generated with an older
analyzer implementation. The current static-runtime report is therefore the authoritative blocker
list; its counts are not a like-for-like comparison with that older 349-miss snapshot.

| Image | Implemented | Missing | Largest unresolved groups |
|---|---:|---:|---|
| `GLFWDemo.exe` | 162 | 42 | USER32 21; GDI32 10; KERNEL32 8; SHELL32 3 |
| `GraphicsEngineD3D11_64r.dll` | 107 | 14 | KERNEL32 10; D3DCOMPILER_47 2; SHLWAPI 1; USER32 1 |
| `Archiver_64r.dll` | 100 | 12 | KERNEL32 9; D3DCOMPILER_47 2; SHLWAPI 1 |
| `D3Dcompiler_47.dll` | 103 | 28 | KERNEL32 15; ADVAPI32 12; RPCRT4 1 |
| **Aggregate** | **472** | **96** | |

The static runtime leaves no reported MSVCP140, VCRUNTIME140, or UCRT import misses in the EXE,
engine, and archiver. The EXE's current misses include 21 USER32, 10 GDI32, 8 KERNEL32, and 3
SHELL32 symbols; the engine retains 14 imports, the archiver 12, and D3DCompiler 28. The archived
JSON records each unresolved name.

### Third-party DLL loading audit

The current Release artifacts were inspected with the PE parser. `GLFWDemo.exe`,
`GraphicsEngineD3D11_64r.dll`, `Archiver_64r.dll`, and `D3Dcompiler_47.dll` each have a TLS
directory, but `Pe::tls_callbacks()` returns an empty list for all four. Their export tables contain
no forwarded exports. These binaries therefore do not exercise either callback or forwarder
handling.

A 30,000,000-instruction AnyWASM replay with `--mode d3d11` reached `user32!CreateWindowExW` and
`d3d11!D3D11CreateDevice`, then executed code in the bundled D3D11 engine and `D3Dcompiler_47.dll`
before reaching the step cap. This confirms that the selected target maps and executes code in those
DLLs; it does not verify their initialization order. Source inspection shows `load_image` reserves a
module before recursively loading its imports, while `run_dll_inits` invokes `DllMain` in module
creation order. The engine and compiler DLLs have nonzero entry points; their process-attach calls
returned successfully in that order. `Archiver_64r.dll` also has an entry point, but its runtime load
was not observed. The initialization order is not dependency-first. Static TLS data is initialized,
but the loader does not invoke `Pe::tls_callbacks()`. Named forwarded exports are resolved opportunistically;
ordinal forwarders are dropped, while failed named forwarders are omitted and only fail if later
imported.

The selected artifacts leave the callback and forwarder cases untested. Use a representative DLL
that contains those features before claiming support; separately verify dependency-first `DllMain`
ordering against that target.

### Bounded AnyWASM trial

The EXE was run with the Release output directory as its VFS root so the companion DLLs and shader
assets were available, with `--mode d3d11` and a 3,000,000-instruction cap. The first traced run
reached static CRT command-line/code-page initialization, then faulted while trying to write 512
bytes to address zero in `kernel32!MultiByteToWideChar`. Argument tracing captured
`(1252, 1, src, 256, NULL, 0xbf00be00000000)`: `cchWideChar` is a 32-bit `int`, so its low 32 bits
were zero (a valid count-only query), while unused upper stack-slot bits were dirty. The HLE had read
the full 64-bit slot as the capacity and tried to write the conversion result to the null output
pointer.

The HLE now reads both signed length parameters from their low 32 bits, preserves
`cbMultiByte=-1` as the null-terminated input form, rejects `cbMultiByte=0` and negative output
capacity with `ERROR_INVALID_PARAMETER`, returns `ERROR_INVALID_PARAMETER` for a null output pointer
with nonzero capacity, and returns `ERROR_INSUFFICIENT_BUFFER` for a short non-null buffer. A new
freestanding guest fixture covers these cases. Microsoft documents `cchWideChar=0` as a count-only call that does not use the output pointer,
`cbMultiByte=-1` as including the terminating NUL, and lists both
`ERROR_INSUFFICIENT_BUFFER` and `ERROR_INVALID_PARAMETER` among possible errors
([API documentation](https://learn.microsoft.com/en-us/windows/win32/api/stringapiset/nf-stringapiset-multibytetowidechar)).
The x64 calling convention assigns arguments beyond the fourth to eight-byte stack slots, so the HLE
must honor the declared 32-bit parameter type when reading those values
([ABI documentation](https://learn.microsoft.com/en-us/cpp/build/x64-calling-convention?view=msvc-170)).
The new fixture was built locally with the installed VS 2022 `cl`/`link`; `fixtures/build.sh` now
includes it in the existing clang/lld-link fixture build. Native Windows probes returned
`ERROR_INVALID_PARAMETER` (87) for a null output with nonzero capacity, both with `cbMultiByte=-1`
and with an explicit byte count of 256. A null output with `cchWideChar=0` returned the required
size (256), and a non-null undersized buffer returned `ERROR_INSUFFICIENT_BUFFER` (122). The guest
regression follows those observed results; Microsoft's error list includes both codes but does not
map each code to each null-buffer case.

After that fix, the same bounded game run passed the conversion calls and reached a second null
write in `kernel32!LCMapStringEx`. The last captured call was
`(NULL, 512, src, 256, NULL, 0xbf00be00000000)`: `cchDest` is again a zero 32-bit count-only query,
but at the time of that trace the LCMap HLE read the full stack slot and tried to write 512 bytes
to address zero.
Microsoft documents `cchDest=0` as a count-only query that does not use `lpDestStr`
([API documentation](https://learn.microsoft.com/en-us/windows/win32/api/winnls/nf-winnls-lcmapstringex)).
That LCMap count-query fault has since been addressed. At this earlier checkpoint the trace passed
the `LCMapStringEx` call and reached thread creation. Thread creation executed finite callbacks,
then the pre-scheduler runtime stopped explicitly at `kernel32!SleepConditionVariableSRW` because
it could not resume a blocked guest thread. No GLFW window or D3D11 renderer was observed at that
checkpoint. GLFW's Win32 window-creation path is also known from
source inspection to call `SHELL32!DragAcceptFiles` unconditionally; that later call remains
unreached and its behavior has not been verified in this target.

### Current checkpoint after scheduler, CPU, and API slices

The cooperative scheduler and synthetic regressions cover retaining and resuming guest contexts for
condition-variable waits. An earlier bounded run stopped at `kernel32!SleepConditionVariableSRW`
because its blocked guest continuation could not be resumed. A fresh current-tree 30,000,000-step CLI
replay reached the same call with `nested=0`, logged 13 `WakeAllConditionVariable` calls afterward,
continued into D3DCompiler, and hit the instruction cap. Reproduction from the repository root:

```powershell
$release = 'D:\Dev Proj\WASM\target-eval\DiligentBuild-GLFWDemo-staticcrt\DiligentSamples\Samples\GLFWDemo\Release'
.\target\debug\anywasm.exe run "$release\GLFWDemo.exe" --dir $release --trace-imports --max-steps 30000000 -- --mode d3d11
```

The trace is in `%TEMP%\anywasm-glfwdemo-bounded-recheck.log`. This run shows guest execution after
the wait but does not prove that the blocked thread itself resumed. No host GLFW window or D3D11
renderer was observed; the higher-cap guest traces below are separate evidence. Target-grounded
static TLS support assigns the image's TLS slot from index zero, initializes private blocks from each
module template, and saves/restores `TEB+0x58` per thread. The freestanding
`static_tls_thread_test.exe` verifies template initialization and worker/main isolation.

The next trace stopped at `GLFWDemo.exe+0x96d8` with a read fault at `0x4`. Its sequence loaded the
TLS vector from `GS:[0x58]`, loaded slot zero, then dereferenced a null block. After the TLS fix, the
trace advanced to `GLFWDemo.exe+0x36b3`, bytes `C4 E3 7D 19 C0 01` (`VEXTRACTF128 xmm0,ymm0,1`). A
target-specific implementation now extracts the upper 128-bit lane, and the following indirect
callback runs.

With the same 30,000,000-step cap, the target reaches virtual `CreateWindowExW`,
`D3D11CreateDevice`, swap-chain/resource setup, repeated condition-variable waits, and
`PathFileExistsW` for `C:\RenderStates.json`. A prior replay then reached `ExitProcess(3)` after
late-bound library resolution. Adding `advapi32!SystemFunction036` and `kernel32!RtlPcToFileHeader`
moved the trace beyond that branch. The current replay reaches `kernel32!RaiseException` with code
`0xe06d7363`, flags `1`, four arguments, and caller `0x14006c3bf` (`GLFWDemo.exe+0x6c3bf`). The
exception parameters contain MSVC C++ EH magic `0x19930520`, an exception object, ThrowInfo at
`0x1400dcb08`, and image base `0x140000000`; the ThrowInfo type descriptors identify
`std::runtime_error` and `std::exception`.

PE32+ exception tables are now parsed and validated; this image has 2,092 `.pdata` records, including
2,089 version 1 and 3 version 2 unwind records. Target-grounded frame search invokes the image's
language handler, and second-pass unwinding through `RtlUnwindEx` resumes at the C++ catch. A
synthetic regression checks handler calls, active unwind-stack use, flags, target RIP/RSP, and return
value. The bounded target replay confirms the catch path.

The next imports are `advapi32!RegOpenKeyExA` probes for
`Software\Microsoft\Direct3D\Direct3D12` under predefined roots `HKCU` and `HKLM`, with options
`0` and access `0x20019`. The HLE currently has no registry data store, so both non-empty paths
return `ERROR_FILE_NOT_FOUND` (`2`). A replay capped at 100,000,000 instructions then remains in a
repeating stream of `kernel32!HeapAlloc` and `kernel32!TlsGetValue` imports until the step cap. This
does not establish why the target has not reached shader creation.

At this earlier checkpoint the trace did not reach shader creation or `Present`. Although the HLE
created virtual window, device, and swap-chain objects, no host window, host graphics device,
displayed frame, or game acceptance had been verified.

Reproduction used the Release output directory as the VFS root, `--trace-imports`, and
`--max-steps 100000000`. The import trace was captured in the task environment under
`%TEMP%\anywasm-glfwdemo-regopen-100m.log`; it is not a repository artifact.

### Current checkpoint: guest compute, draw, and present calls reached

Target-grounded CPU regressions now cover additional instructions seen in `GraphicsEngineD3D11_64r.dll`:
`VCMPEQSS` at RVA `0x17239`, `VBLENDVPS` at `0x1723e`, `VMOVD` at `0x17244`, 64-bit `SHRX` at
`0x40b1a`, `BLSI` at `0x4e9f0`, and `VPEXTRD` at `0x4ef0b`. The decoders accept only the observed
forms and have focused register, memory, flag, and vector-lane tests.

The SDF setup's null texture path was traced to `CheckFormatSupport`: Diligent selects a texture
format only when the returned flags include `D3D11_FORMAT_SUPPORT_TYPED_UNORDERED_ACCESS_VIEW`.
The HLE now advertises that bit for `DXGI_FORMAT_R32_FLOAT` only, creates 2D and 2D-array UAV views,
records `CSSetUnorderedAccessViews` (including supplied initial counts), and records D3D11.1
`*SetConstantBuffers1` ranges in 16-byte shader-constant units. Tests cover format selection, UAV
mip-slice tracking, UAV command encoding, and constant-buffer range encoding. The browser executor
records these bindings and aligns short or misaligned constant-buffer ranges for WebGPU.

After these slices, a D3D11-only `/MT` replay reached `CreateComputeShader`,
`CSSetShaderResources`, `CSSetSamplers`, `CSSetUnorderedAccessViews`, and the guest
`ID3D11DeviceContext3::Dispatch`. It then reached the first-frame `Draw` and
`IDXGISwapChain2::Present` calls. The trace contains no new unimplemented-import stop. The replay used
`--max-steps 1000000000` and was manually interrupted after repeated guest presents to stop the
unbounded demo loop; it did not exit normally or reach the step cap. The trace is in the task
environment at `%TEMP%\anywasm-glfwdemo-cb1-3b.log` and is not a repository artifact.

This is guest-side progress: the CLI records HLE commands, but does not execute them through the
browser's WebGPU executor. A separate browser command-stream fixture now verifies a 4x4 R32_FLOAT
UAV dispatch, reads back the `0.25` values, and samples the texture in a draw with pixel checks.
That validates the narrow typed-UAV executor path; it does not run the selected target's command
stream or verify its SDF shader, host-visible GLFW Demo pixels, or gameplay. The 96 static missing
imports, registry data store, and tutorial D3D11 paths also remain open.

### Guest instruction cost per present

The process-wide `retired_instructions` counter now includes instructions executed in synchronous
`Process::call_guest` callbacks, HLE instructions that return a frame stop, and successfully retired
instructions before a later instruction in the same cached block faults. The JS wrapper requires
the `aw_retired_instructions` export and fails clearly for a stale WASM module instead of falling
back to the active-thread `aw_icount` counter.

A synchronous callback still runs on the host stack and cannot suspend. If its callback does not
return before the current `Process::run` budget expires, the runtime returns a diagnostic crash
without exceeding that run budget; it cannot resume that callback on a later call. A GLFW Demo run
with 10,000,000-instruction slices stopped at 10,000,000 instructions in such a callback. The frame
benchmark therefore defaults to one run slice equal to its full instruction cap. This keeps the
representative run bounded while giving its synchronous callbacks enough budget to return. It does
not remove the runtime's continuation limitation for callers that deliberately request smaller
slices.

The `test/target_frame_bench.mjs` benchmark drives the target until guest `Present` frame stops,
reads the process-wide counter, and drains the recorded GPU stream at each stop. Its fixture
regression requests four stops from `d3d11_triangle.exe`; it records all three frames followed by
clean exit. A second fixture run caps execution at one guest instruction and confirms that no frame
is recorded. Measurement starts after `.awb` loading; guest DLL initialization performed during
loading is outside the frame deltas and elapsed frame times.

The corrected representative runs used Node `v24.16.0` on Windows x64 and the release WASM from
the accounting-fix working tree. The target bundle is the eight-file output of the D3D11-only `/MT`
Release build documented above. Each run requested eight frames under a 1,000,000,000-instruction
cap, with the default slice size equal to that cap. Reproduction:

```powershell
$release = 'D:\Dev Proj\WASM\target-eval\DiligentBuild-GLFWDemo-staticcrt\DiligentSamples\Samples\GLFWDemo\Release'
$bundle = "$env:TEMP\anywasm-glfwdemo-frame-measure.awb"
.\target\debug\anywasm.exe pack "$release\GLFWDemo.exe" -o $bundle --dir $release
node test/target_frame_bench.mjs $bundle --frames 8 --max-instructions 1000000000 --jit-min-prefix 65
node test/target_frame_bench.mjs $bundle --frames 8 --max-instructions 1000000000 --jit-min-prefix 1
```

The packed bundle contained 8 files (7,963,698 bytes); its SHA-256 matched an independent repack.
Both runs reached all eight guest `Present` stops, stayed below the cap, and did not reach a
terminal state. Frame 1 includes the main guest initialization performed after bundle loading;
frames 2–8 are subsequent presents. Counts are process-wide guest instructions. Times are Node wall
time between the post-load start and each frame stop:

| MOV prefix | First frame instructions / time | Frames 2–8 instructions each | Frames 2–8 time range / mean | Total instructions / elapsed | Emitter stats |
|---:|---:|---:|---:|---:|---|
| 65 (default, cached-only) | 580,777,731 / 42,168.83 ms | 3,200 | 0.214–0.482 ms / 0.262 ms | 580,800,131 / 42,170.67 ms | 0 compilations, 0 executions, 0 fallbacks |
| 1 (forced emission) | 580,776,966 / 67,687.26 ms | 3,200 | 0.242–0.527 ms / 0.293 ms | 580,799,366 / 67,689.33 ms | 687,197 compilations, 13,087,664 executions, 0 fallbacks; 11,494,229 register-register hits |

Forced emission made the cold first-frame pass 60.5% slower in this run. Only the existing
leading-MOV prototype was emitted; it made no fallback and did not improve this target sample. A
cached-only rerun of the same final WASM and bundle used 580,777,093 first-frame instructions
(580,799,493 total) and took 40,430.06 ms for the first frame, 638 fewer instructions and about
1.74 s faster than the table's cached-only run. This small sample does not characterize the source
of that variation. After the atexit-budget fix, the rebuilt WASM was measured again with the same
bundle and command: cached-only retired 580,771,924 instructions / 51,371.36 ms on frame 1
(580,794,324 total), while prefix 1 retired 580,774,467 / 76,179.10 ms (580,796,867 total).
Both repeat runs reached eight frame stops, with 3,200 instructions on each later frame, and had no
terminal state or cap hit. The latest cold emitted pass was about 48.3% slower than its cached-only
pair; elapsed times varied substantially across repeats, so this remains diagnostic data rather
than a stable performance comparison. The
benchmark runs the guest and HLE in Node; draining the stream does not execute D3D11 commands
through browser WebGPU. It measures neither host-rendered frame time nor target FPS, shader pixels,
gameplay, or a product performance target. The older 10–50x performance estimate remains
unverified.

Pinned upstream references: [DiligentSamples README](https://github.com/DiligentGraphics/DiligentSamples/blob/03a10e15e69c5ae1738a52175ae029f3f4275a90/README.md),
[GLFW Demo source](https://github.com/DiligentGraphics/DiligentSamples/tree/03a10e15e69c5ae1738a52175ae029f3f4275a90/Samples/GLFWDemo),
[GLFW Demo CMake](https://github.com/DiligentGraphics/DiligentSamples/blob/03a10e15e69c5ae1738a52175ae029f3f4275a90/Samples/GLFWDemo/CMakeLists.txt),
[GLFW license](https://github.com/glfw/glfw/blob/master/LICENSE.md), and
[Microsoft D3DCompiler redistribution guidance](https://learn.microsoft.com/en-us/gaming/gdk/docs/features/common/packaging/packaging-framework-packages?view=gdk-2604),
[Microsoft Visual C++ redistribution terms](https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files?view=msvc-170).

### Browser attempt with the selected Diligent target

On 2026-10-11, the rebuilt `web/anywasm_wasm.wasm` and the D3D11-only `/MT` `GLFWDemo.awb`
were served to headless Chromium. The normal worker run slice is 250,000 instructions; this
attempt stopped after 8,524,265 aggregate guest instructions. The diagnostic was
`synchronous guest callback exhausted the run budget; its continuation cannot be resumed`.
The synchronous callback path still cannot suspend and resume across worker slices.

For diagnosis only, a Playwright response route temporarily changed the served worker's run
slice to 1,000,000,000 instructions; no repository runtime code was changed for this probe. The
guest advanced through its first roughly 580-million-instruction frame and the browser observed
30 or more guest `Present` stops. WebGPU reported invalid render pipelines followed by invalid
command buffers. A 1040x807 readback contained only transparent black pixels. The page did not
provide a successful completion status, and no target image was verified. The first rejected shader
and validation cause are now captured below.

The first rejected shader was guest shader id 16, a 1,636-byte `GenerateSDF.hlsl` compute shader
(SHA-256 `c8927fda2387000f58620450f2ffed95935283ea4794a224cc8d173fca0c7900`). Its SPIR-V
contained an f32 `OpConstant` with bits `0xfffffff8`, originating from the HLSL integer loop
initializer `l(-8)` in the Microsoft DXBC disassembly. SPIR-V permits this bit pattern, but Naga 30
rejects it as an invalid constant expression. `web/shaders.js` now preserves those bits through an
integer local and runtime bitcast; `test/shaders.mjs` regresses the captured DXBC. In a later bounded
Chromium replay, the compute shader module and pipeline validated and one dispatch executed.

At this checkpoint, render-pipeline validation failed because the translated vertex shader used
Naga's hidden `@group(3) @binding(0)` draw-parameter uniform while the graphics pipeline layout had
no binding there. The vendored Naga manifest defines four i32 values (`baseVertex`, `baseInstance`,
and two reserved values). Its generated WGSL subtracts the first two from WebGPU's built-in vertex
and instance indices to reproduce the D3D system values. The earlier 1040x807 readback contained
only transparent black pixels. The normal worker slice still stopped in a synchronous callback that
cannot resume.

### Browser replay after draw-parameter binding

`web/shaders.js` now reserves group 3 binding 0 for Naga's uniform and maps D3D UAV register `u#` to
binding `# + 1`. `web/gpu.js` writes the per-draw base vertex and first instance into a 16-byte
uniform allocation in the staging ring. `test/shaders.mjs` checks the translated bindings, and
`test/browser/run.mjs` renders two indexed instanced draws with different nonzero offsets; the
second draw must update the per-draw values for its green center pixel. The existing R32_FLOAT UAV
dispatch/readback/sample regression still passes after the binding shift.

The reusable target smoke replay is `test/browser/target.mjs`. It temporarily serves the worker with
a 1,000,000,000-instruction run slice and per-frame GPU reporting, then captures the first reported
back buffer from a caller-supplied target bundle. Run it after packing the D3D11-only `/MT` build:

```powershell
$release = 'D:\Dev Proj\WASM\target-eval\DiligentBuild-GLFWDemo-staticcrt\DiligentSamples\Samples\GLFWDemo\Release'
$bundle = "$env:TEMP\anywasm-glfwdemo-frame-measure.awb"
.\target\debug\anywasm.exe pack "$release\GLFWDemo.exe" -o $bundle --dir $release
$env:CHROMIUM = 'C:\Users\sysadmin\AppData\Local\ms-playwright\chromium-1234\chrome-win64\chrome.exe'
$env:REQUIRE_TARGET_MOVEMENT = '1'
$env:REQUIRE_TARGET_MAP_RESET = '1'
node test/browser/target.mjs $bundle docs\images\glfwdemo-first-frame.png --check-movement --check-map-reset --check-uav
```

On 2026-10-11, the latest Chromium 151.0.7922.34 replay produced a capture at 1040x807 with 5 draws,
1 compute dispatch, 5 guest presents, 10 render/compute passes, 2 pipelines, and no WebGPU validation errors.
The readback had 651,249 nontransparent, nonblack pixels; the center sample was RGBA `(18, 255, 26,
255)`. Visual inspection shows the dark SDF maze scene, blue wall marks, and green player marker:

![Diligent GLFW Demo WebGPU frame](images/glfwdemo-first-frame.png)

The browser worker measures the time spent waiting for `requestAnimationFrame` after each guest
`Present` and adds that elapsed duration to the virtual guest clock in 100 ns units. This lets the
target's `QueryPerformanceCounter`-based frame delta advance while preserving the instruction-based
clock during guest execution.

The same target replay checks input through the rendered output. GLFW creates a hidden helper window
before the visible game window, so routing browser input to the first top-level HWND silently dropped
keys. The runtime now selects the focused visible window, or falls back to a visible top-level window.
With this fix, holding W for 500 ms moves the green player marker 2.5 pixels upward. Pressing Tab
regenerates the map; 165,601 of 839,280 pixels changed (19.7%), and the target issued a second compute
dispatch. This reset path exposed Diligent's `ID3D11DeviceContext3::Flush1` followed by an idle-event
wait and an event-query polling loop. `Flush1` now signals its optional guest event, and D3D11 event
queries report completion after `End` in the ordered browser command-stream model. Rust regressions
cover hidden-helper input selection, idle-event signaling, and event-query completion. The browser
run reached 128 presents and 2 dispatches with no WebGPU validation errors. The W and Tab captures
are saved at [after W](images/glfwdemo-first-frame.after-w.png) and
[after Tab](images/glfwdemo-first-frame.after-tab.png).

![GLFW Demo after W moves the player](images/glfwdemo-first-frame.after-w.png)

![GLFW Demo after Tab regenerates the maze](images/glfwdemo-first-frame.after-tab.png)

### Deterministic native comparison

For a matched maze layout, a temporary D3D11-only `/MT` build used maze seed 1 and teleport seed 2;
the source edit was restored after the build. The native D3D11 run rendered at 1024x768. A browser
replay of the same bundle rendered at 1040x807, with 6 draws, 1 compute dispatch, 6 initial presents,
12 passes, 2 pipelines, and no WebGPU validation errors. The native and browser captures are checked
in as [native](images/glfwdemo-native-seed1.png) and
[WebGPU](images/glfwdemo-webgpu-seed1.png) images.

The deterministic maze seed is reused on each map construction, so Tab produces the same maze in
this bundle. The separate map-reset assertion uses the original random-seed build, where it verifies
the regenerated map pixels and second compute dispatch.

The capture hashes are SHA-256 `FF9CFF060A7179E9D489F13A76C6F9AA46DE3EDB88B9C1E9FAE163FFD4C07A3C`
(native) and `9007C1DFD3ACEB3EA2776B653C6C46BAA45106BEFC57614C587194F878B58C84` (WebGPU). Centered
crop comparison with FFmpeg gave 40.936 dB PSNR for 256x256 crops (browser origin 392,275; native
origin 384,256), and 35.014 dB PSNR with 0.897977 SSIM for the 128x128 scene-focus crops (browser
origin 456,339; native origin 448,320). Visual inspection confirms the same seed-1 wall layout,
player, and teleport. These are bounded visual comparisons: the backbuffer sizes and capture phases
differ, and they do not establish pixel equivalence or verify gameplay.

### Independent SDF UAV readback

`test/browser/target.mjs --check-uav` copies the selected compute UAV texture to a mapped buffer and
checks the output independently of the rendered backbuffer. The same seed-1 replay read view
12/resource 10 as a 128x128 `r32float` texture: all 16,384 values were finite, 3,776 were negative
and 12,608 positive, with a minimum of -2.236068 and maximum of 4.0. The target shader uses
`RADIUS=8` and `DIST_SCALE=0.5`, so the observed range is within its signed ±4 distance bound.
Sampled values at the top-left, center, and bottom-right were -1.414214, 2.0, and -1.414214. The
report is saved in [`target-data/glfwdemo-sdf-seed1.json`](target-data/glfwdemo-sdf-seed1.json).
The standalone 4x4 R32_FLOAT fixture also reads all 16 written values as 0.25. This verifies the
selected target's SDF output for one deterministic state; it does not establish other UAV
formats/readback paths, all gameplay states, or game acceptance.

This verifies one movement direction and map regeneration in the selected target. Its deterministic
seed-1 SDF UAV has a bounded independent readback; other movement directions and turning are
unverified, and no full gameplay or real-game acceptance pass is claimed. The default
250,000-instruction worker slice still cannot resume the synchronous guest callback; the larger slice
is diagnostic test routing only.

## Next evidence needed

- Verify movement in all directions and turning, then complete a controlled playthrough before
  making a gameplay acceptance claim. Other UAV formats and general staging readback remain untested.
- Make the selected target's synchronous guest callbacks suspend and resume across normal worker
  slices; the large-slice replay does not address the default browser scheduling limit.
- Extend the virtual registry only from observed target paths and required values. The current
  `RegOpenKeyExA` slice supports predefined roots and missing-key results; it does not provide
  registry query, create, write, or persisted-value behavior.
- Continue the tutorial traces from the reached `MSVCP140.dll` `basic_ios<char>` constructor only
  after defining the required C++ stream-state semantics; each tutorial still has many other static
  MSVC runtime imports. Their current D3D11 traces do not reach shader creation or Present.
- Keep Asteroids outside the redistributable acceptance set until the Intel-origin code and sample
  media have a documented licence review.
- The D3D11-only GLFW Demo remains the selected game candidate. The current `/MT` analyzer report has
  96 missing imports across the EXE and three DLLs; the archived prior report had 122 after static
  linking. Conversion, version, path, scheduler, static TLS, selected target VEX/BMI, secure random,
  PE unwind primitives, the selected C++ catch path, D3D11 UAV binding, and constant-buffer ranges
  have regressions. The latest 1,000,000,000-step trace was interrupted after it reached guest
  compute dispatch, draw, and repeated present calls. The browser target replay now verifies host
  rendering, W movement, and Tab map regeneration; native comparison, independent UAV readback, and
  gameplay acceptance remain unverified.
