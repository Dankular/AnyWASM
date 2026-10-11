# Status (work in progress)

## Verified (tests pass)
- `anywasm-pe`: PE32/PE32+ parser (headers, imports, delay-imports, exports, relocations, TLS).
  9 tests, expected values cross-checked against `llvm-readobj` on clang/lld-link-built fixtures
  (`fixtures/build.sh`).
- `anywasm-cpu`: paged memory + decoder + integer/SSE/SSE2/SSE3/SSSE3/SSE4.1/SSE4.2/x87 interpreter.
  **Differential test against the host CPU** (`cargo test -p anywasm-cpu --test diff`): 2,730
  corpus entries (assembled by llvm-mc via `tools/gen_corpus.py`) x 40 random trials each, comparing
  GPRs, flags, XMM and memory. All match. The harness found and fixed 8 real defects (xmm shift
  counts, pcmpestr lengths, 16-bit double-shift hardware behaviour, dpps/dppd sum order, fcomi
  flags, fprem quotient bits and invalid cases, f80 conversion underflow).

## Known limits of the CPU core (not hidden)
- x87 is modelled with f64, not 80-bit extended. Transcendentals (fsin, fcos, fptan, fpatan, f2xm1,
  fyl2x, constants) are only compared to 1e-9 relative tolerance against hardware.
- rcpps/rsqrtps (and ss forms) return exact reciprocals, not the hardware's approximations
  (excluded from the diff corpus).
- Not decoded: VEX/EVEX (AVX, BMI), MMX, AES/PCLMUL, 16-bit/segment modes. CPUID hides them.
- The diff corpus covers single instructions and x87 sequences. Control flow (jcc/call/ret/loop),
  push/pop/enter/leave and `syscall` are not in it (rsp is not comparable) and are only exercised
  by unit tests / PE programs.
- The interpreter has no decode cache or JIT; speed has not been measured.

## Not started
- PE loader + Win64 HLE, CLI (analyze/pack), WASM crate and browser harness, D3D/WebGPU.
