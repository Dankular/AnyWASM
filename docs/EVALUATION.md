# Prior-art evaluation

Question: does code already exist that we can reuse instead of writing an x86-64 CPU core, a PE
loader and a D3D→WebGPU layer? Everything below was **read or run in this environment**; claims
taken from a README and not independently checked are marked *(README)*.

## AnyPS5 (the technique we are borrowing)

Source: <https://github.com/boykopovar/AnyPS5> (GPL-2.0), docs read: `README.md`,
`docs/dev/ARCHITECTURE.md`, `docs/dev/BUILD.md`.

- No emulation: PS5 code is x86-64 and runs natively. A *relinker* rewrites the executable into an
  ELF/PE, binds imports **by NID** (hashed function name) to reimplemented system libraries
  (`core/libs/prx`), and filters unused NIDs.
- GPU shaders: RDNA → IR → SPIR-V recompiler with an in-memory and on-disk cache.
- Unsupported states throw `std::runtime_error`; coverage is partial (one verified title).

What transfers to Windows→WASM: *bind imports by name to reimplemented libraries*, *recompile
shaders*, *fail loudly on gaps* (`Stop::Unimplemented{dll,name}` here). What does **not** transfer:
"no CPU emulation" — Windows binaries are x86-64 but a browser is not, so we need a CPU.

## Candidates (cloned and inspected)

| Project | License | Result |
|---|---|---|
| **WineBrowser** `evangit2/winebrowser` | MIT | Active (last commit 2026-10-07), ~91k lines JS. x86 basic blocks → WASM in the browser, Wine PE DLLs as guest modules, D3D9/D3D12 (vkd3d-shader + Naga) and Vulkan rendered through WebGPU. **PE32 (32-bit) only**: `src/pe.js` fails unless machine is `0x14c`; README lists x64 under current exclusions. Own unit suite: **1330 / 1343 pass** here; all 13 failures are Windows-1252 decoding mismatches (`'A\x80B'` vs `'A€B'`), consistent with Node lacking full ICU in this container (not confirmed). README reports app start-up of ~61–98 s. |
| **retrowin32** `evmar/retrowin32` | Apache-2.0 | **Abandoned** (HEAD: "mention abandonment, point to theseus"). 32-bit only; its `pe` crate rejects non-i386. The "x86-64" doc is about running 32-bit guests on x86-64 hosts. x86 interpreter ≈ 5.4k lines, Win32 layer ≈ 41.8k lines Rust. |
| **Theseus** `evmar/theseus` | not stated | Static x86→source translation (32-bit examples). Author says WASM "should easily extend" but it is not implemented. Not inspected beyond the author's post. |
| **Canary** `Dankular/Canary` | MIT/Apache | x86-64 Linux ELF emulator → WASM. ~12.5k lines Rust, **zero tests** (`cargo test` reports 0 in every crate); ~150 mnemonics in the decoder enum — no packed-float SSE, packed integer math, or `pshufb` visible there. Not run on real binaries. |
| v86 / v86_64, Boxedwine/ExeBrowser, Wine-Assembly, ax | various | Found by search only, **not inspected**. v86_64 per its README: no SSSE3/SSE4/AVX. |
| CheerpX | proprietary | Not usable. |

No open-source **D3D11→WebGPU** or **DXBC→WGSL** project was found. WineBrowser's D3D path goes
through vkd3d-shader + Naga; Dankular/VkWebGPU-ICD provides Vulkan→WebGPU (README claims; not run).

## Decision

1. Both existing PE loaders reject PE32+, so a 64-bit loader is new work either way
   (`anywasm-pe` + `anywasm-rt`).
2. WineBrowser's CPU is a JS WASM-emitter for 32-bit x86; extending it to x64 is a "separate
   architectural task" by its own TASKS.md. We built a standalone Rust x86-64 core instead and
   verify it against real hardware (see STATUS.md) — something none of the inspected projects
   report doing.
3. The graphics layer is where WineBrowser is genuinely ahead; it is the strongest reuse
   candidate once guests are 64-bit (struct layouts and pointer sizes differ, so it is not drop-in).
