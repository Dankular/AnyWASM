# Status (work in progress)

## Verified (tests pass)
- `anywasm-pe`: PE32/PE32+ parser (headers, imports, delay-imports, exports, relocations, TLS).
  9 tests, expected values cross-checked against `llvm-readobj` on clang/lld-link-built fixtures
  (`fixtures/build.sh`).
- `anywasm-cpu::mem` and `anywasm-cpu::decode`: 14 tests pass (paged memory, decoder).

## Written but NOT yet compiled or tested
- `anywasm-cpu::{exec, sse, x87}`: integer/SSE/x87 interpreter. Never built. Planned verification:
  differential testing against the host CPU. Known deviations: x87 uses f64, rcpps/rsqrtps are exact.
- `lib.rs` module wiring for exec/sse/x87 was not applied (the edit was interrupted).

## Not started
- PE loader + Win64 HLE, CLI (analyze/pack), WASM crate and browser harness, D3D/WebGPU.

## Open decision
Prior art exists (retrowin32, WineBrowser, v86_64, Boxedwine). Not yet evaluated; see the
conversation. The custom CPU core may be dropped in favour of one of them.
