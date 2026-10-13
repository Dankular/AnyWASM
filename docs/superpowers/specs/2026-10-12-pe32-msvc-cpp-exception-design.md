# Native MSVC C++ Exception Support for PE32

**Status:** EH3 metadata search and bounded second-pass catch transfer are implemented. A native
x86 fixture verifies one caller/callee catch transfer with inner-frame destructor execution. The
broader compatibility goal remains active across Win32, PE32, DLLs, graphics, CPU features, browser,
and packaging.

## Goal

Handle native MSVC C++ throws and catches in x86 PE32 images when the guest C++ runtime DLL is
absent. Reuse the PE32 linked registration chain and the existing normalized C++ type-matching
behavior where possible, while adding the x86-specific metadata reader and dispatcher transfer
needed by MSVC's registration-based ABI. Keep the current x64 EH3/EH4 implementation unchanged in
behavior.

## Current evidence

- `exception32.rs` dispatches through the x86 TEB registration chain, validates stack bounds and
  cycles, and invokes each handler with the exception record, registration node, context record,
  and a null fourth argument. For a selected C++ catch it now performs a bounded second pass,
  invokes handlers with `EXCEPTION_UNWINDING` and the target flag, removes registrations above the
  target, and resumes from the target context. Nested/collided dispositions remain unimplemented.
- `Process::run` treats an escaped `GuestUnwind` as a crash. The x86 exception dispatcher must
  consume its own nonlocal transfers before returning to the main run loop.
- `msvc_eh.rs` implements relative-pointer x64 EH3/EH4 metadata. PE32 dispatch now branches to
  `msvc_eh32.rs` for absolute-pointer EH3 metadata and the three-parameter x86 throw record.
- The installed MSVC 14.29.30133 `ehdata.h` defines the i386 registration node as `pNext`, `frameHandler`,
  and signed `state`, with a saved stack pointer immediately before the node. The same header selects
  absolute pointers for non-relative `FuncInfo`, `TryBlockMapEntry`, `HandlerType`, and throw type
  metadata. Its x86 `DispatcherContext` is a `void` placeholder, and its x86 exception record has
  three language parameters rather than the four used on x64.
- Microsoft documents that `__CxxFrameHandler3` receives an `EHRegistrationNode *`; its context and
  dispatcher-context arguments are unused on Intel processors. The handler therefore has to derive
  function state and metadata from x86 registration data and the exception record, not x64
  `DISPATCHER_CONTEXT` fields.
- A compile-only x86 build of the existing fixture with MSVC 19.29.30159 and `/d2FH4-` confirmed the
  actual per-function thunk: it reads the registration node, performs its stack-cookie checks, loads
  the absolute `FuncInfo` address into EAX, then jumps to `__CxxFrameHandler3`. The runtime uses EAX
  for the function metadata and the registration node's `state` field for the active state.

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
`0xe06d7363` through the PE32 exception dispatcher with the three x86 language
parameters: magic `0x19930520`, thrown-object pointer, and `ThrowInfo` pointer. Do not append the
x64-only image-base parameter. Preserve noncontinuable exception behavior and report malformed
metadata as a guest diagnostic.

The `__CxxFrameHandler3` path reads PE32 absolute-pointer EH3 metadata and uses the registration
node's current state. The compile-only fixture established that its compiler thunk supplies
`FuncInfo` in EAX; it is not passed through an x64-style dispatcher context. A selected catch is
recorded during search, then the dispatcher performs a second pass over registrations through the
target, runs EH state actions, unlinks frames above the target, and installs the saved target stack
and catch IP in the x86 `CONTEXT`. A native x86 fixture now verifies one caller/callee catch transfer
and confirms the inner-frame destructor runs once.

The first-pass handler searches active try blocks in compiler order, matches typed catches and
catch-all only for native C++ exceptions, and records a target catch together with its registration
frame. The dispatcher records the ordered registration frames and exception context, validates that
the requested target appears in that search, and consumes the transfer internally. Extend the PE32
dispatcher with a bounded second-pass unwind over the frames it searched. That pass sets
`EXCEPTION_UNWINDING` on the record and adds `EXCEPTION_TARGET_UNWIND` for the target registration
node, invokes frame handlers while walking toward that node, and runs C++ state actions in reverse
construction order. Because `Process::call_guest` restores its caller CPU context when it propagates
`GuestUnwind`, the dispatcher must install the saved x86 transfer context explicitly after cleanup.
Remove registrations above the target from the TEB chain, keep the target node linked, update its
state to the selected catch scope, construct the catch object, and transfer the guest context to the
selected catch funclet. Keep the existing SEH disposition rules and vectored-handler ordering. Any
nested exception raised by a guest copy constructor, destructor, or catch funclet must go back through
normal guest exception dispatch; an unwind transfer must not escape to `Process::run`.

All metadata reads use checked 32-bit guest addresses, image ownership checks, bounded table counts,
and checked arithmetic. Validate registration nodes against TEB stack bounds and the saved chain,
and reject cycles, invalid states, truncated tables, out-of-image pointers, and non-executable
funclets. Unsupported metadata or catch features stop with a specific diagnostic rather than
silently selecting a handler.

## Compatibility and error behavior

- The current implementation slice covers MSVC x86 EH3 metadata and the dispatcher path for local
  or outer-frame `/EHsc` catches. Build the fixture with EH4 disabled so it exercises
  `__CxxFrameHandler3` explicitly.
- x64 PE32+ EH3/EH4 parsing and dispatch remain on the existing code path.
- `/EHa` conversion of arbitrary CPU faults into C++ catches, CLR/WinRT exceptions, native x86 EH4,
  exception specifications, `exception_ptr`, and future compiler encodings remain out of scope.
  The installed MSVC 14.29 and 14.44 `ehdata_forceinclude.h` headers set
  `_EH_RELATIVE_FUNCINFO` to `0` for native x86; EH4's relative function metadata path is enabled
  for x64/ARM targets. Keep PE32 C++ FH4 unsupported until a native x86 image demonstrates a
  concrete handler thunk and metadata ABI.
- One direct cross-registration-frame catch transfer and inner-frame destructor are fixture-verified.
  Other frame layouts remain untested. X86 catch-copy functions use the guest `__thiscall` ABI with
  checked destination and source ranges. On a selected catch, the PE32 handler now calls the
  executable ThrowInfo unwind function exactly once after catch-object setup and before restoring
  the catch continuation; the fixture verifies the thrown object stays alive through a by-reference
  catch and is destroyed after the body. Nested/collided SEH unwind dispositions remain unsupported.
- Unwind funclets execute as guest code and lower their registration state before running. A nested
  exception caught inside the destructor is allowed to complete. If it escapes the cleanup frame,
  the runtime calls `std::terminate`, even when an outer C++ handler could match it. The active EH3
  registration frame distinguishes local cleanup catches from handlers outside the destructor.
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
  nested, by-value/by-reference, destructor, and one cross-registration-frame case. Native and
  HLE-only direct-entry and normal-startup runs return matching zero exit results.
- Separate native x86 fixtures verify that a destructor exception escaping during stack unwinding
  terminates before an outer catch runs and when no handler matches. HLE-only CLI and Chromium runs
  report the `std::terminate` crash; the existing fixture with a locally caught nested exception
  continues to exit 0.
- Existing PE32 SEH/VEH tests, the x64 EH3/EH4 fixture suite, `cargo test -p anywasm-rt`, and
  `cargo build --workspace` remain green.

## References

- [Microsoft `__CxxFrameHandler3` reference](https://learn.microsoft.com/en-us/windows/win32/memory/cxxframehandler3)
  documents the registration-node arguments and states that context and dispatcher-context inputs
  are unused on Intel processors.
- [C++ draft, exception handling](https://eel.is/c%2B%2Bdraft/except.throw) says that an exception
  escaping a destructor invoked during stack unwinding calls `std::terminate`.
- Local MSVC 14.44 headers `ehdata.h` and `ehdata_values.h` define the x86 registration node,
  absolute-pointer EH3 metadata, exception-record parameter count, and supported EH magic values.
- Existing x64 design: `docs/superpowers/specs/2026-10-12-msvc-cpp-exception-support-design.md`.
