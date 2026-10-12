# Native MSVC C++ Exception Support for PE32

**Status:** design for the next PE32 exception-runtime workstream. The broader compatibility
goal remains active across Win32, PE32, DLLs, graphics, CPU features, browser, and packaging.

## Goal

Handle native MSVC C++ throws and catches in x86 PE32 images when the guest C++ runtime DLL is
absent. Reuse the PE32 linked registration chain and the existing normalized C++ type-matching
behavior where possible, while adding the x86-specific metadata reader and dispatcher transfer
needed by MSVC's registration-based ABI. Keep the current x64 EH3/EH4 implementation unchanged in
behavior.

## Current evidence

- `exception32.rs` dispatches through the x86 TEB registration chain, validates stack bounds and
  cycles, and invokes each handler with the exception record, registration node, context record,
  and a null fourth argument. It currently treats nested/collided dispositions as unimplemented and
  does not consume `GuestUnwind`.
- `msvc_eh.rs` implements relative-pointer x64 EH3/EH4 metadata and `_CxxThrowException`; its throw
  entry explicitly rejects PE32.
- The installed MSVC 14.44 `ehdata.h` defines the i386 registration node as `pNext`, `frameHandler`,
  and signed `state`, with a saved stack pointer immediately before the node. The same header selects
  absolute pointers for non-relative `FuncInfo`, `TryBlockMapEntry`, `HandlerType`, and throw type
  metadata. Its x86 `DispatcherContext` is a `void` placeholder, and its x86 exception record has
  three language parameters rather than the four used on x64.
- Microsoft documents that `__CxxFrameHandler3` receives an `EHRegistrationNode *`; its context and
  dispatcher-context arguments are unused on Intel processors. The handler therefore has to derive
  function state and metadata from x86 registration data and the exception record, not x64
  `DISPATCHER_CONTEXT` fields.

## Design

Add an x86-specific EH layer in `msvc_eh32.rs`, alongside the existing x64
`msvc_eh.rs`. It parses the absolute-pointer EH3 structures emitted for PE32 and normalizes them
into the existing catch-type model. Keep layout readers, pointer validation, registration-frame
handling, and architecture-specific catch-frame calculations separate from x64 code. Share only
architecture-neutral type matching and catch-object operations after their interfaces are verified
to fit both address models.

Extend `_CxxThrowException` to accept PE32 stdcall arguments. Validate the `ThrowInfo` and its
metadata pointers against the owning loaded image, validate copy/unwind routines as executable guest
addresses, and validate the thrown-object range as readable guest memory. Then raise exception
`0xe06d7363` through `exception32::dispatch_raised_exception` with the three x86 language
parameters: magic `0x19930520`, thrown-object pointer, and `ThrowInfo` pointer. Do not append the
x64-only image-base parameter. Preserve noncontinuable exception behavior and report malformed
metadata as a guest diagnostic.

Implement `__CxxFrameHandler3` for PE32 using the registration node's current state and the
function's EH3 `FuncInfo`. Before implementation of the reader, a small native x86 fixture must
confirm how the supported MSVC compiler version makes each function's `FuncInfo` reachable through
the handler call. The implementation must follow that emitted ABI and the installed header layouts;
it must not infer an x64-style dispatcher context or assume that the registration node alone holds
all function metadata.

The first-pass handler searches active try blocks in compiler order, matches typed catches and
catch-all only for native C++ exceptions, and records a target catch together with its registration
frame. Extend the PE32 dispatcher with a bounded second-pass unwind over the frames it searched.
That pass sets `EXCEPTION_UNWINDING` on the record and adds `EXCEPTION_TARGET_UNWIND` for the target
registration node, invokes frame handlers while walking toward that node, and runs C++ state actions
in reverse construction order. Remove registrations above the target from the TEB chain, keep the
target node linked, update its state to the selected catch scope, construct the catch object, and
transfer the guest context to the selected catch funclet. Keep the existing SEH disposition rules
and vectored-handler ordering. Any nested exception raised by a guest copy constructor, destructor,
or catch funclet must go back through normal guest exception dispatch.

All metadata reads use checked 32-bit guest addresses, image ownership checks, bounded table counts,
and checked arithmetic. Validate registration nodes against TEB stack bounds and the saved chain,
and reject cycles, invalid states, truncated tables, out-of-image pointers, and non-executable
funclets. Unsupported metadata or catch features stop with a specific diagnostic rather than
silently selecting a handler.

## Compatibility and error behavior

- The first implementation covers MSVC x86 EH3 metadata and the native `/EHsc` model. Build the
  fixture with EH4 disabled so it exercises `__CxxFrameHandler3` explicitly.
- x64 PE32+ EH3/EH4 parsing and dispatch remain on the existing code path.
- `/EHa` conversion of arbitrary CPU faults into C++ catches, CLR/WinRT exceptions, x86 EH4,
  exception specifications, `exception_ptr`, and future compiler encodings remain out of scope.
- By-value copy constructors, destructors, unwind funclets, and catch funclets execute as guest
  code. Faults from that code are dispatched normally; the HLE must not swallow them.
- An unmatched C++ exception continues through outer registration frames, then follows the existing
  top-level filter and unhandled-exception behavior.

## Verification

- Parser tests cover valid x86 EH3 function, try-block, handler, unwind, throw, and catchable-type
  structures; malformed absolute pointers, state indices, counts, strings, and table ranges are
  rejected.
- Dispatcher tests cover a typed catch, derived-to-base catch, by-reference aliasing, catch-all,
  unmatched propagation to an outer frame, nested try blocks, and destructor order during target
  and intervening-frame unwind.
- A native MSVC x86 `/MD /EHsc` fixture built with EH4 disabled covers typed, base, catch-all,
  nested, by-value/by-reference, and destructor cases. Require matching native and emulator exit
  results both with the runtime DLLs present and with HLE-only imports.
- Existing PE32 SEH/VEH tests, the x64 EH3/EH4 fixture suite, `cargo test -p anywasm-rt`, and
  `cargo build --workspace` remain green.

## References

- [Microsoft `__CxxFrameHandler3` reference](https://learn.microsoft.com/en-us/windows/win32/memory/cxxframehandler3)
  documents the registration-node arguments and states that context and dispatcher-context inputs
  are unused on Intel processors.
- Local MSVC 14.44 headers `ehdata.h` and `ehdata_values.h` define the x86 registration node,
  absolute-pointer EH3 metadata, exception-record parameter count, and supported EH magic values.
- Existing x64 design: `docs/superpowers/specs/2026-10-12-msvc-cpp-exception-support-design.md`.
