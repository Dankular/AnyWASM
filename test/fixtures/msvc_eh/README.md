# MSVC C++ exception fixtures

`build.ps1` uses the latest installed Visual Studio C++ toolchain and writes five fixtures under
the ignored `target/msvc-cpp-eh/` directory. The EH4 compile prints `cl /Bv`, including the
compiler and linker versions, so a run can be tied to the toolchain that emitted the metadata.

- `eh4-dynamic.exe` uses the toolset's default FH4 metadata.
- `eh3-dynamic.exe` adds `/d2FH4-` to request legacy FH3 metadata.
- `eh3-x86-dynamic.exe` is a PE32 build with the normal MSVC CRT startup and legacy FH3 metadata.
- `eh3-x86-direct.exe` is a PE32 build using legacy FH3. It enters `main` directly to isolate the
  C++ EH path from the CRT startup's EH4 wrapper.
- `seh-x86.exe` exercises PE32 C `__try`/`__except`/`__finally` with the normal MSVC CRT startup.
- The binaries dynamically import the MSVC/UCRT runtime; the script does not copy runtime DLLs.

The C++ fixtures return 0 only when all paths pass. Their return codes identify the failed case:

| Exit code | Case |
|---:|---|
| 10 | Catch derived exception by value |
| 20 | Catch derived exception through an adjusted base reference |
| 30 | Catch a second exception with `catch (...)` |
| 40 | Catch through nested try blocks |
| 50 | Run the local destructor during exception unwind |
| 60 | Catch across a registration frame and run the inner-frame destructor |
| 71 | Nontrivial catch-by-value object has the wrong copied value |
| 72 | Nontrivial catch-by-value copy constructor did not run |
| 81 | Caller stack pointer changed after catching a derived object by value |
| 82 | Caller stack pointer changed after catching through a base reference |
| 83 | Caller stack pointer changed after catching with `catch (...)` |
| 84 | Caller stack pointer changed after nested catches |
| 85 | Caller stack pointer changed after local destruction during unwind |
| 86 | Caller stack pointer changed after catching across a registration frame |
| 87 | Caller stack pointer changed after nontrivial catch-by-value |
| 91 | Thrown object did not match the selected handler |
| 92 | Thrown-object destructor ran before the catch body completed |
| 93 | Catch-all handler selected instead of the typed handler |
| 94 | Thrown-object destructor did not run exactly once after the catch |

`seh-x86.exe` returns 0 only when filter selection, exception continuation, nested exception handling,
and both normal and unwind-time termination handlers pass:

| Exit code | Case |
|---:|---|
| 1 | Catch the exception code or run the normal `__finally` block |
| 2 | A `CONTINUE_SEARCH` filter incorrectly enters its handler |
| 3 | The outer handler fails to catch after the inner filter continues search |
| 4 | A `CONTINUE_EXECUTION` filter incorrectly enters its handler |
| 5 | Execution does not resume after `RaiseException` |
| 6 | The outer handler fails to catch or the unwind-time `__finally` does not run once |
| 7 | A filter-raised nested exception or the original exception is not caught |
| 8 | The escaping nested exception is not caught by the outer handler |
| 9 | The filter does not run once for each nested exception |
| 10 | The exception raised from `__finally` does not replace the in-flight exception |
| 11 | The nested filter unexpectedly enters its own handler |
| 12 | The colliding `__finally` handler does not run exactly once |

Build and inspect the imported exception entry points from the repository root:

```powershell
.\test\fixtures\msvc_eh\build.ps1
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\eh4-dynamic.exe
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\eh3-dynamic.exe
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\eh3-x86-dynamic.exe
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\eh3-x86-direct.exe
.\target\debug\anywasm.exe analyze .\target\msvc-cpp-eh\seh-x86.exe
.\target\debug\anywasm.exe run .\target\msvc-cpp-eh\eh3-x86-dynamic.exe --max-steps 2000000
.\target\debug\anywasm.exe run .\target\msvc-cpp-eh\eh3-x86-direct.exe --max-steps 2000000
.\target\debug\anywasm.exe run .\target\msvc-cpp-eh\seh-x86.exe --max-steps 2000000
```

To run the five fixtures through the browser runtime without MSVC/UCRT DLLs, build the browser WASM
from the current source and run the Chromium smoke test:

```powershell
cargo build -p anywasm-wasm --target wasm32-unknown-unknown --release
node scripts/asyncify-wasm.mjs
Copy-Item target\wasm32-unknown-unknown\release\anywasm_wasm.asyncified.wasm web\anywasm_wasm.wasm
$env:CHROMIUM = 'C:\path\to\chrome.exe'
node test/browser/exception-fixtures.mjs
```

The test packs each executable by itself, uploads it through the page's `.awb` input, and requires a
normal process exit with code 0. It needs the fixtures built by `build.ps1` and `playwright-core`
installed from `test/browser/package.json`.

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

On 2026-10-13, the two PE32 C++ fixtures and `seh-x86.exe` built with the installed x86 MSVC toolchain
and returned 0 under a 2,000,000-step HLE-only run cap. The direct-entry C++ fixture exercised
by-value and adjusted-reference catches, catch-all, nested try/catch, and destructor execution during
unwind. The x86 catch funclets returned their post-catch continuation addresses through EAX; the
runtime calls the funclet with its parent frame and resumes at that continuation. The C SEH fixture
verified `EXCEPTION_CONTINUE_SEARCH`, `EXCEPTION_CONTINUE_EXECUTION`, exception-code access in an
`__except` block, normal `__finally`, and `__finally` during frame unwind. The runtime decodes the
EH4 registration record and cookie-encoded scope table, validates local cookies with the image's
`__security_check_cookie`, and transfers to the selected handler through the PE32 dispatcher. This
also verifies a filter that raises and catches a second exception before selecting the original
exception. It verifies that an uncaught exception raised in a filter reaches the outer handler, and
that an exception raised from `__finally` replaces the in-flight exception while the termination
handler runs exactly once. Both cases return 0 natively and through the HLE-only CLI. A synthetic
runtime regression separately exercises a handler returning `EXCEPTION_NESTED_EXCEPTION` and checks
that the next registration sees `EXCEPTION_NESTED_CALL`; the end-to-end fixture does not return
disposition 2 or 3. Full nested-frame bookkeeping, explicit collided-unwind disposition handling, and
every compiler-specific EH4 layout remain open.

On 2026-10-13, the rebuilt release WASM and Asyncify module ran all five fixture packages in
`test/browser/exception-fixtures.mjs` under Chromium. Every package exited 0 and the page reported no
JavaScript errors. The `seh-x86` package includes the filter-raised and `__finally` exception cases
above.

On 2026-10-13, the C++ fixture added exit code 60 for a cross-registration-frame transfer: a noinline
callee owns a `DestructorProbe` and throws `Derived`, while its caller catches `Base` by reference and
checks the inner destructor ran exactly once. The native `eh3-x86-direct.exe`, HLE-only emulation of
that direct-entry image with a 2,000,000-step cap, and HLE-only emulation of the normal-startup
`eh3-x86-dynamic.exe` all returned 0. This verifies this direct caller/callee catch path; it does not
cover arbitrary frame layouts, nested/collided unwind dispositions, or custom catch-copy closures.

The `ThrowLifetimeProbe` case catches the exception by reference, checks that its destructor has not
run inside the catch body, then requires exactly one destructor call after the handler exits. The
native x86 direct/startup and x64 EH3/EH4 fixture binaries all return 0. HLE-only x86 direct/startup
and x64 EH3/EH4 fixture runs also return 0. The x86 handler calls the executable unwind function
recorded in `ThrowInfo` after the catch funclet returns and before it installs the catch continuation.
