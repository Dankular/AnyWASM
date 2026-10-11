# Win64 unwind primitives implementation plan

- [x] Add checked `RUNTIME_FUNCTION` parsing for PE32+ exception directories and parser tests.
- [x] Retain exception table RVA/size on each loaded guest `Module`; implement binary-search lookup and
   `RtlLookupFunctionEntry` with success and null-output cases.
- [x] Add Win64 context accessors and an unwind-info interpreter for documented version 1 operations,
   leaf returns, chained records, handler data, and establisher-frame output.
- [x] Add focused synthetic tests for every supported operation and explicit unsupported/malformed
   failures; keep `RaiseException` fail-loud.
- [x] Rebuild, run focused and workspace Rust tests, then replay the bounded GLFW Demo and update
   `CLAUDE.md` and `docs/TARGETS.md` with the observed checkpoint and remaining dispatch gap.

## Review focus

- Verify prologue unwind-code filtering uses the control-PC offset and descending code order.
- Ensure all RVA arithmetic is bounded by the owning mapped image and chained records cannot loop.
- Preserve guest register/context state when metadata is malformed or unsupported.
- Distinguish a missing function-table entry (leaf function) from invalid image metadata.
