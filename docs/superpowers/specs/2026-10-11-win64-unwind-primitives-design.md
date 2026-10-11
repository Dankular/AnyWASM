# Win64 unwind primitives

## Purpose

The selected D3D11 GLFW Demo now reaches a guest `RaiseException` carrying MSVC's C++ exception
code after `RtlPcToFileHeader` succeeds. The current runtime stops at that import. The exception
uses the PE image's `.pdata`/`.xdata` tables, so implement the Win64 table lookup and virtual unwind
primitives as a verifiable foundation for the target's still-open SEH/C++ dispatch work.

## Scope and boundary

- Parse the PE32+ exception data directory as sorted `RUNTIME_FUNCTION` records.
- Retain each loaded guest image's exception-table range and implement `RtlLookupFunctionEntry`.
- Implement `RtlVirtualUnwind` for leaf frames and version 1 unwind metadata using push, allocation,
  frame-register, saved GPR/XMM, machine-frame, and chained-info operations.
- Apply the Win64 `CONTEXT` register layout and return language-handler metadata to guest outputs.
- Reject unsupported versions, malformed ranges, unknown opcodes, cyclic chains, and unsupported
  epilogue contexts with an explicit `Stop::Unimplemented`/`Stop::Crash`; do not silently guess.
- Keep `RaiseException` fail-loud until a separate dispatcher can search frames, invoke language
  handlers, and perform second-pass unwinding. This slice does not claim C++ catch support.

## Design

`anywasm-pe` exposes bounded, checked exception-directory records. `Module` stores the directory's
guest virtual range, allowing the runtime to binary-search records already mapped in guest memory.
The lookup API returns the record's guest address and optional image base. `RtlVirtualUnwind` reads
the record and `UNWIND_INFO` from guest memory, updates only the defined portion of the supplied
Win64 context, and writes handler data and establisher frame outputs when requested. Its unwind
engine is a pure helper over `Process` memory and has synthetic tests for each supported opcode and
failure mode. No host exception mechanism or platform ABI is involved.

## Verification

- PE parser tests cover sorted records, empty directories, malformed sizes, and truncated data.
- Runtime tests cover lookup by PC and virtual unwinding of leaf, prologue/body, chained, GPR, XMM,
  handler, and machine-frame cases, plus malformed/unsupported metadata.
- The selected target is replayed with the existing 30,000,000-step cap. Its next result is recorded
  as evidence only; passing these primitives is not a claim that exception dispatch, rendering, or
  game acceptance works.

## References

- [Microsoft x64 exception handling](https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64)
- [Microsoft `RtlVirtualUnwind`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/nf-winnt-rtlvirtualunwind)
