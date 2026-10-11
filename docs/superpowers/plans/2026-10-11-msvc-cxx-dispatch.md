# Targeted MSVC C++ exception dispatch plan

Spec: [2026-10-11-msvc-cxx-dispatch-design.md](../specs/2026-10-11-msvc-cxx-dispatch-design.md)

1. [x] Add trace-only stack-window diagnostics at `RaiseException`; capture the selected target's
   actual caller and return-address chain under the existing bounded replay.
2. [x] Verify the handler ABI, image-relative handler data, catch target, and `RtlUnwindEx` call
   against the selected EXE and installed Microsoft x64 headers/documentation.
3. [x] Add bounded guest exception-record/context construction and first-pass frame search using
   the supported unwind primitives; invoke the target language handler with a synthetic
   `DISPATCHER_CONTEXT` and preserve the live thread context during search.
4. [x] Implement the target-observed second-pass transfer, including `RtlUnwindEx` control-flow
   handoff if the guest handler requests it. Keep unknown dispositions, unsupported metadata, and
   other exception forms fail-loud.
5. [x] Add focused synthetic coverage and validate against the selected guest C++ throw/catch path;
   run workspace tests and the bounded selected-target replay. A separate fixture was unnecessary
   because the selected Diligent binary provides the target-grounded guest path.
6. [x] Update `CLAUDE.md`, `docs/TARGETS.md`, and the runtime GitHub task queue with the exact
   resulting checkpoint. Do not claim rendering or gameplay acceptance without observing it.
