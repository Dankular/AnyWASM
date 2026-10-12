# MSVC C++ exception fixtures

`build.ps1` uses the latest installed Visual Studio x64 C++ toolchain and writes two `/MD /EHsc`
executables under the ignored `target/msvc-cpp-eh/` directory. The EH4 compile prints `cl /Bv`,
including the compiler and linker versions, so a run can be tied to the toolchain that emitted the
metadata.

- `eh4-dynamic.exe` uses the toolset's default FH4 metadata.
- `eh3-dynamic.exe` adds `/d2FH4-` to request legacy FH3 metadata.
- The binaries dynamically import the MSVC/UCRT runtime; the script does not copy runtime DLLs.

The executable returns 0 only when all paths pass. Return codes identify the failed case:

| Exit code | Case |
|---:|---|
| 10 | Catch derived exception by value |
| 20 | Catch derived exception through an adjusted base reference |
| 30 | Catch a second exception with `catch (...)` |
| 40 | Catch through nested try blocks |
| 50 | Run the local destructor during exception unwind |

Build and inspect the imported exception entry points from the repository root:

```powershell
.\test\fixtures\msvc_eh\build.ps1
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\eh4-dynamic.exe
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\eh3-dynamic.exe
```

These fixtures cover the minimum compiler-generated records needed to start EH3/EH4 runtime work;
they do not cover virtual bases, `/EHa`, `exception_ptr`, or unknown future EH4 encodings.

## Local build evidence

Built on 2026-10-12 with MSVC 19.44.35214.0 and linker 14.44.35214.0. Both native executables
returned 0. `dumpbin /imports` confirmed that the EH4 executable imports `_CxxThrowException` and
`__CxxFrameHandler4`; the EH3 executable imports `_CxxThrowException` and `__CxxFrameHandler3`.
Neither executable imports `MSVCP140.dll` or bundles runtime DLLs. The analyzer snapshot reported
four unresolved CRT imports in each fixture: `_register_thread_local_exe_atexit_callback`, `terminate`,
`__current_exception`, and `__current_exception_context`. The analyzer was rebuilt after the HLE
work and now reports 46/46 imports implemented for each image, with no statically unresolved
imports. FH3 imports seven VCRUNTIME140 functions; FH4 imports six VCRUNTIME140 functions and one
VCRUNTIME140_1 function. Both also import the CRT heap, locale, math, runtime, and stdio API sets and
15 KERNEL32 functions. Analyzer coverage does not imply that the real runtime DLL exception path
works.

The local EH4 metadata reader also normalized five compiler-generated handler records from the EH4
fixture. Their unwind-map entry counts were 2, 2, 2, 4, and 3; the records contained 1, 1, 1, 2, and
1 try blocks. This checked the relative FuncInfo pointer and maps emitted by the installed compiler.

Disassembly of `exception_fixture.obj` confirms the funclet ABI used by these records: catch and
destructor funclets read their parent frame from `RDX`; catch funclets return a continuation address
in `RAX`. The runtime phase-two path calls those funclets after state unwind and uses the returned
continuation when restoring the target frame. A catch selected from a try body uses the establisher
frame directly; EH3 consults `dispFrame` only when execution is already inside a catch funclet.

On 2026-10-12, both fixtures ran with an HLE-only VFS containing only the executables and a
2,000,000-step cap. `eh3-dynamic.exe` and `eh4-dynamic.exe` each returned 0 and reached `crt!exit`.
Their traces exercised by-value derived catches, adjusted base-reference catches, catch-all, nested
try/catch, and unwind funclets. Neither run stopped on an unresolved C++ runtime import.

A second runtime VFS contained the installed x64 VC143 CRT redist DLLs from
`C:\Program Files\Microsoft Visual Studio\2022\Professional\VC\Redist\MSVC\14.44.35112\x64\Microsoft.VC143.CRT`,
`C:\Windows\System32\ucrtbase.dll` (10.0.22621.3593), and copies of both fixtures. With the
current-source CLI, both fixtures returned 0 under the same 2,000,000-step cap and reached
`ExitProcess(0)`. Their import traces include guest calls from `ucrtbase.dll` and `vcruntime140.dll`;
the EH4 run also executes `vcruntime140_1.dll`. This confirms DLL initialization and the runtime
exception path with the installed UCRT present. API-set contracts without a physical DLL in the VFS
continue to use HLE. These local DLLs are probe inputs, not product dependencies or a redistribution
decision. The exact trace captures are ignored files under `target-trace/msvc-eh-runtime-vfs-*`.
