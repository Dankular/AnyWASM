# Native MSVC C++ Exception Support for PE32+

**Status:** implementation design for the first exception-runtime workstream. The broader
compatibility goal remains active across PE32, Win32, DLLs, graphics, CPU features, browser, and
packaging.

## Goal

Handle native MSVC C++ throws and catches in x64 PE images when the C++ runtime DLL is absent from
the guest package. Use the existing Windows exception dispatcher and guest unwinder so C++
exceptions share the same vectored handlers, frame walk, termination behavior, and context restore
as other exceptions.

## Current evidence

- `crates/anywasm-rt/src/hle/crt.rs` maps `__CxxFrameHandler3`, `__CxxFrameHandler4`,
  `_CxxThrowException`, and `__std_terminate` to an explicit unimplemented stop.
- `crates/anywasm-rt/src/exception.rs` recognizes exception code `0xe06d7363`, validates its
  four-parameter x64 record, and dispatches frames through the existing PE unwind path.
- The selected Diligent samples render with the installed MSVC/UCRT DLLs. The HLE-only Tutorial03
  browser replay also reaches repeated frames without an unresolved import; the remaining HLE C++
  personality gap is not reached by that sample replay.
- The installed MSVC 14.44 headers define the relative EH3 structures in `ehdata.h` and describe
  the compressed EH4 record format in `ehdata4_export.h`. The implementation must validate all
  guest offsets and counts before reading them.

## Design

Add a focused `msvc_eh` runtime module. It owns parsing and validation of MSVC throw metadata,
versioned function metadata, type matching, catch-object construction, and C++ unwind actions. The
existing `exception.rs` module remains the owner of the process-wide first-pass frame walk,
vectored handlers, target unwind, and final context restoration.

`_CxxThrowException(object, throwInfo)` creates the standard noncontinuable MSVC C++ exception
record (`0xe06d7363`) with the magic value, object address, ThrowInfo address, and containing PE
image base. It enters the existing raised-exception dispatcher. Invalid image ownership, metadata,
or exception-record layouts fail loudly with a guest diagnostic.

`__CxxFrameHandler3` and `__CxxFrameHandler4` read the function's language-specific handler data
from `DISPATCHER_CONTEXT`, find the active try state from the control PC, and search its handlers in
compiler order. Typed catches use the thrown object's CatchableType list, TypeDescriptor names,
qualifiers, and PMD base adjustment. Catch-all catches match only native C++ exceptions. A selected
catch constructs its destination object using the metadata's copy operation or a validated simple
copy, then transfers through the existing guest unwind continuation. State unwind actions run in
reverse construction order before the catch body starts.

EH3 parsing supports the relative FuncInfo and type metadata emitted by supported MSVC x64 images.
EH4 parsing uses its compressed integer maps and sparse handler records. Parsers reject truncated,
overflowing, cyclic, or out-of-image data. Unknown metadata versions and unsupported catch features
return a specific `Stop::Unimplemented` instead of silently accepting an incorrect catch.

## Compatibility and error behavior

- Existing `RaiseException`, CPU-fault translation, VEH ordering, `__C_specific_handler`,
  `RtlUnwindEx`, vectored-continue handlers, and top-level filters retain their current behavior.
- C++ personality support is guest-mode aware. This design implements x64 PE32+ metadata; x86
  registration-based C++ EH remains a later part of the broader PE32 workstream.
- Managed CLR exceptions, `/EHa` conversion of arbitrary hardware faults to C++ catches, WinRT
  exception metadata, `exception_ptr`, and unknown future EH4 encodings remain explicit gaps.
- C++ copy constructors, destructors, and catch funclets execute as guest code. Their faults re-enter
  the existing nested exception dispatcher; they are not swallowed by the HLE.

## Verification

- Unit fixtures cover EH3/EH4 parsing, malformed ranges and counts, catch ordering, typed and
  catch-all matching, base-subobject adjustment, by-reference and by-value catch objects, and
  destructor/unwind ordering.
- A small MSVC `/MD` fixture exercises `_CxxThrowException`, a typed catch, a base catch, a
  catch-all, nested try blocks, and local-object destruction without loading MSVCP/VCRUNTIME DLLs.
  Its result must match a native execution and the real guest return code.
- The same fixture run with the MSVC runtime DLLs loaded checks that the existing guest-runtime path
  remains compatible.
- Existing `cargo test -p anywasm-rt` and workspace build remain green.

## References

- [Microsoft x64 exception handling](https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64)
  defines the x64 language-handler call interface and the dispatcher context passed to a personality.
- [Microsoft `__CxxFrameHandler3` reference](https://learn.microsoft.com/en-us/windows/win32/memory/cxxframehandler3)
  identifies the CRT personality entry point.
- Local MSVC 14.44 headers: `ehdata.h`, `ehdata_forceinclude.h`, and `ehdata4_export.h` under the
  installed Visual Studio toolset. These headers are the source for the versioned EH3/EH4 layouts
  used by the local compiler fixture.
