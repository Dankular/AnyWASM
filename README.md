# AnyWASM

Run Windows x64 programs inside WebAssembly. The design borrows the idea behind
[AnyPS5](https://github.com/boykopovar/AnyPS5): don't emulate an operating system, **execute the
program's own machine code and bind every import by name to a reimplemented system library**.
Because a browser is not an x86 machine, the CPU part is an interpreter (`anywasm-cpu`) that is
tested against real hardware; the system libraries are written in Rust and compiled into the same
WASM module.

> **Status: early.** Console programs run, including real mingw-compiled programs that import the
> actual `msvcrt.dll` API, and a **Direct3D 11 program renders through WebGPU** (verified
> pixel-for-pixel on a coloured triangle). There is **no audio, no threads, no GDI**, the D3D11
> surface is a vertical slice, and the interpreter is ~20 M instructions/s (games need 10-50x more). See
> [docs/STATUS.md](docs/STATUS.md) for exactly what is verified and what is not, and
> [docs/EVALUATION.md](docs/EVALUATION.md) for the prior-art research (WineBrowser, retrowin32, ...).

## Layout

| Crate / dir | What it is |
|---|---|
| `crates/anywasm-pe` | Dependency-free PE32/PE32+ parser |
| `crates/anywasm-cpu` | x86-64 interpreter (integer, SSE..SSE4.2, x87), paged memory, differential-tested vs the host CPU |
| `crates/anywasm-rt` | Process runtime: PE64 loader, TEB/PEB, import thunks, host kernel32/ntdll/C runtime, VFS, bundles |
| `crates/anywasm-cli` | `anywasm analyze | run | pack` |
| `crates/anywasm-wasm` | C-ABI WebAssembly build (no wasm-bindgen) |
| `web/` | Browser page, worker, JS glue, WebGPU executor (`gpu.js`), shader toolchain (`shaders.js` + vendored vkd3d-shader/Naga WASM, see `web/shaders/licenses`) |
| `fixtures/` | Test executables (clang/lld-link and mingw-w64 builds) and `build.sh` |
| `tools/gen_corpus.py` | Generates the differential-test instruction corpus via `llvm-mc` |

## Try it

```sh
cargo run -p anywasm-cli -- analyze fixtures/crt_test.exe     # which imports are implemented?
cargo run -p anywasm-cli -- run fixtures/crt_test.exe -- a "b c"
./web/build.sh && node web/serve.mjs 8080                       # then open http://localhost:8080/?run=hello
```

`analyze` reports, per DLL, which imported functions have a host implementation. Anything missing
is bound to a trap that stops the program with the exact `dll!function` (never a silent guess).

## Tests

```sh
cargo test                       # unit, differential-vs-hardware, end-to-end .exe runs
node test/wasm_smoke.mjs         # the same executables through the WASM build (needs web/build.sh)
node test/browser/run.mjs        # real page in headless Chromium (npm install in test/browser first)
```

## License

GPL-2.0-only (see `Cargo.toml`). Third-party research material is referenced, not copied.

## Legal scope

AnyWASM does not provide Windows programs or game assets. You must supply binaries and assets that
you obtained lawfully and are authorized to use. Review `THIRD_PARTY.md` before redistributing a
build that includes the vendored shader tools.
