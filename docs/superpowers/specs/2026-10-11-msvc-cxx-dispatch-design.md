# Targeted MSVC C++ exception dispatch

## Target evidence

The selected D3D11-only, `/MT` Diligent GLFW Demo throws through `kernel32!RaiseException` with
code `0xe06d7363`, flags `1`, and parameters
`[0x19930520, exception_object, 0x1400dcb08, 0x140000000]`. The caller is
`GLFWDemo.exe+0x6c3bf`, inside the version 1 runtime-function range
`0x6c328..0x6c3cf`, which has no language handler. The image's nearby ranges
`0x6c3d0..0x6c424` and `0x6c424..0x6c478` use handler RVA `0x6d2f4`. `dumpbin /disasm` shows this
handler reads `DISPATCHER_CONTEXT` fields and checks the C++ exception code; PE type descriptors
identify the thrown object as `std::runtime_error` and its base `std::exception`.

The trace-only stack window records `RSP=0x7ff0000ffa60`. The HLE must model the imported
`RaiseException` call as having transferred control to the caller: set the initial control PC to
`[RSP]` (`0x6c3bf`) and the exception context's stack pointer to `RSP+8`. Unwinding the throwing
helper `0x6c328..0x6c3cf` yields return PC `0x2be2`; unwinding the MSVC throw helper
`0x2bc0..0x2be3` yields return PC `0x53db4`. The latter is in function `0x53c50..0x53db5`, whose
version 1 unwind record has EHANDLER and UHANDLER flags and handler RVA `0x6af74`.
`dumpbin /disasm` confirms `0x6af74` reads `DISPATCHER_CONTEXT.HandlerData` at `+0x38` and
conditionally calls the in-image handler at RVA `0x6d018`. The in-image handler also reads
`ControlPc`, `ImageBase`, `FunctionEntry`, and `ScopeIndex` from the dispatcher context. This is the
first actual handler path to invoke; `0x4eb01` in the stack window is a saved stack value, not a frame
return address.

The first-pass replay invokes handlers at control PCs `0x140053db4` and `0x140055e49`; both return
`ExceptionContinueSearch` (`1`). A later guest handler calls `RtlUnwindEx` with
`TargetFrame=0x7ff0000ffe68`, `TargetIp=0x140056bbe`, an exception record at
`0x7ff0000ff3b0`, `ReturnValue=0`, a context record at `0x300059060`, and no history table. This is
the observed second-pass boundary. The installed VS 2022 `ehdata.h` defines the x64 dispatcher
fields through `ScopeIndex` at offset `0x48`; this agrees with the offsets read by the target's
handler code. The installed `excpt.h` defines `ExceptionContinueExecution=0` and
`ExceptionContinueSearch=1`.

The runtime now performs bounded frame search for this C++ exception, invokes the target's language
handler, and completes the observed second-pass unwind through `RtlUnwindEx`. The selected bounded
target replay reaches its guest catch. The image contains three version 2 unwind records; the
current implementation rejects them explicitly if encountered, and this target's walked path did
not require them.

## Goal and boundary

Advance the selected target through its observed MSVC C++ throw by searching guest stack frames and
invoking the target image's own language handler. If that handler selects a catch, perform the
required second-pass unwind and resume at the guest catch. Preserve loud failures for unsupported
exception codes, malformed exception/handler metadata, unsupported unwind versions, stack bounds,
and unimplemented language-handler dispositions. This is not general SEH, vectored exception
handling, host fault translation, or a real-game acceptance claim.

## Design

`RaiseException` recognizes only an MSVC C++ EH record with code `0xe06d7363`, magic
`0x19930520`, four parameters, and an image base/ThrowInfo that resolve inside a loaded guest image.
It creates a guest `EXCEPTION_RECORD` and a snapshot of the Win64 `CONTEXT`. Since AnyWASM replaces
the imported `RaiseException` frame with an HLE, the initial guest context uses `[RSP]` as the
control PC and `RSP+8` as its stack pointer; this mirrors resuming at the guest callsite after
removing the import's return slot.

The dispatcher walks at most a fixed number of frames, with monotonic stack-pointer and module-stack
bounds checks. For each frame it obtains the function entry from module metadata and applies the
already-tested unwind subset to a cloned context. When an exception handler is present, it builds a
guest `DISPATCHER_CONTEXT` and calls the handler using `Process::call_guest`, preserving the live
thread context during search. The context fields must match the Windows x64 layout and the offsets
observed in this target's handler disassembly. Search continues only for the documented
`ExceptionContinueSearch` disposition.

The search records frame, establisher, handler, handler-data, and unwind context for a selected
catch. Phase two follows the target's `RtlUnwindEx` arguments, invokes UHANDLERs through the target
frame, restores the selected frame context, sets the handler's return value, and transfers to its
target IP. The transfer uses an explicit internal non-returning unwind result; ordinary nested
`call_guest` cleanup must not overwrite the selected context.

Trace-only stack words and handler calls in the `RaiseException` diagnostic expose the first caller
frames in the bounded target replay. Synthetic tests cover exception-record construction, frame
progress/bounds, handler dispositions, nested guest calls, and unwind-context transfer. Target
verification completes the C++ catch, then probes `Software\Microsoft\Direct3D\Direct3D12` under
`HKCU` and `HKLM`. With no registry data store, `RegOpenKeyExA` reports those non-empty paths
missing; a 100,000,000-step replay then reaches the cap amid repeated `HeapAlloc`/`TlsGetValue`
imports. No window, host renderer, frame, or gameplay success is inferred from a successful
exception catch.

## References

- [Microsoft x64 exception handling](https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64)
- [Microsoft exception dispatching](https://learn.microsoft.com/en-us/windows/win32/debug/exception-dispatching)
- [Microsoft `RtlUnwindEx`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlunwindex)
- [Microsoft `__CxxFrameHandler3`](https://learn.microsoft.com/en-us/windows/win32/memory/cxxframehandler3)
