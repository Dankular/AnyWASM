# Status (work in progress)

## Verified (tests pass; run `cargo test`)
- `anywasm-pe`: PE32/PE32+ parser. 9 tests, values cross-checked against `llvm-readobj` on
  clang/lld-link-built fixtures (`fixtures/build.sh`).
- `anywasm-cpu`: paged memory, decoder, integer/SSE..SSE4.2/x87 interpreter.
  **Differential test against the host CPU** (`tests/diff.rs`): 2,730 llvm-mc-assembled corpus
  entries x 24 random trials, comparing GPRs, flags, XMM and memory. It has found and fixed 8 real
  defects. It passed on this sandbox's Intel Xeon and on a GitHub-hosted runner's CPU, except for
  one case: 16-bit `shld/shrd` with count > 16, which the SDM calls undefined and vendors differ
  on (the first CI run caught this; that range is now excluded from the corpus and the interpreter
  keeps the Intel behaviour, documented in `exec.rs`).
- `anywasm-rt`: PE64 loader (relocations, imports, delay imports, TLS, TEB/PEB with loader lists,
  bundled DLLs) + host-implemented kernel32/ntdll/CRT. 5 end-to-end tests run **real Windows x64
  executables**: freestanding clang-built programs (console I/O, relocated pointer tables, heap,
  SSE2 math, indirect calls, GetProcAddress) and a mingw-w64 program importing the real
  `msvcrt.dll` (argv, qsort with guest callbacks, printf family, malloc/realloc, math,
  setjmp/longjmp, file I/O on the VFS, sscanf, atexit, exit code, stderr). The mingw program's
  output is identical to the same source compiled natively (checked by hand).
- `printf` engine: unit tests for flags/width/precision/length modifiers/floats.

## Known limits (not hidden)
- x87 is modelled with f64, not 80-bit; transcendentals compared to 1e-9 relative tolerance.
- rcpps/rsqrtps return exact reciprocals (excluded from the diff corpus).
- Not decoded: VEX/EVEX (AVX, BMI), MMX, AES/PCLMUL, 16-bit/segment modes. CPUID hides them.
- Diff corpus covers single instructions and x87 sequences; control flow, push/pop and syscall are
  covered only by the end-to-end programs.
- No threads (CreateThread/_beginthreadex report "unimplemented"), no SEH/C++ exception unwinding,
  no window/GDI/user32, no Direct3D. The interpreter has no decode cache/JIT; speed is unmeasured.
- DLL/function imports with no host implementation are bound to a trap that stops the process with
  `Stop::Unimplemented{dll,name}` (and logs it) instead of guessing.

## Not started
- anywasm CLI (analyze/pack), WASM crate + browser harness, user32/gdi32, D3D/WebGPU, threads, SEH.
- See `docs/EVALUATION.md` for the prior-art evaluation (WineBrowser, retrowin32, ...).
