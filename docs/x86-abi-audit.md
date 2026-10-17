# PE32 import ABI audit

The ABI generator fills the x86 callee-cleanup table in one pass over one or more PE32 files. It
does not create HLE handlers or guess behavior. An import with no real handler remains visible in
`anywasm analyze`; an import whose x86 cleanup size is still unknown stops with
`AbiUnimplemented` when reached.

Build the analyzer, regenerate metadata for the local game executables, then run the focused Python
regressions:

```powershell
cargo build --release -p anywasm-cli
py -3 scripts/generate_win32_abi.py `
  'D:\The Sims 3\Game\Bin\TS3W.exe' `
  'C:\Program Files (x86)\Rockstar Games\GTA San Andreas\gta_sa_compact1.0.exe'
py -3 -m unittest scripts.test_generate_win32_abi
```

The script reads the analyzer's PE import list and local `windows-sys` prototypes. For x86,
`system` arguments use whole four-byte stack slots (with wider values using multiple slots); `C`
imports are caller-clean. The generated file contains only cleanup metadata and fingerprints each
input executable by SHA-256.

Outputs:

- `crates/anywasm-rt/src/x86_abi_generated.rs` — generated cleanup sizes used after reviewed
  hand-written signatures.
- `target/x86-abi-audit.json` — per-target HLE coverage, generated signatures, and prototypes that
  the generator could not resolve. A missing `windows-sys` prototype may already have a reviewed
  hand-written signature; the runtime's fail-closed ABI check is authoritative.

REA MCP supplies the binary evidence pass: inspect the target's import inventory and use callsite
disassembly when a prototype is missing, ambiguous, or inconsistent with observed x86 stack usage.
Record that evidence before adding a hand-written signature. Do not turn an unresolved handler into
a zero-return implementation to make the report green.
