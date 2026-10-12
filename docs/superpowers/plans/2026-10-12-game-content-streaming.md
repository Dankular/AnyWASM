# Game Content Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream GTA data files from a companion `.agp` package while preserving the existing runtime `.wasm` and `.awb` interfaces.

**Architecture:** The VFS stores remote file metadata and performs range reads through an Asyncify-aware host import. The worker coalesces aligned 256 KiB HTTP ranges, keeps a 64 MiB LRU cache, and resumes the suspended guest call after validating the response. The GTA packer and local server produce and serve a raw indexed `.agp` package.

**Tech Stack:** Rust, WebAssembly Asyncify via Binaryen, browser JavaScript workers and Fetch Range, PowerShell package script, Node HTTP server.

**Spec:** `docs/superpowers/specs/2026-10-12-game-content-streaming-design.md`

## Global Constraints

- Keep `gta_sa_compact1.0.exe` as the GTA entry point.
- Keep current `.awb` bundles compatible.
- Use raw uncompressed `.agp` data with 64-bit offsets and lengths.
- Fetch aligned 256 KiB ranges and cap browser cache at 64 MiB.
- Reject servers that do not return valid `206 Partial Content` responses; do not fall back to whole-package fetch.
- Preserve guest file pointer semantics across Asyncify unwind and rewind.

## Review Focus

- Malformed, duplicate, or overflowing package index records must be rejected before creating VFS entries.
- A server that ignores `Range` must fail visibly without buffering the whole package.
- Concurrent requests for bytes in one chunk must coalesce into one fetch.
- Rewinding `ReadFile` must not double-advance the guest file pointer or duplicate guest-memory writes.
- File-backed mapped views must read only the requested view range; reads and writes to separate local save paths must remain usable.

---

### Task 1: Indexed VFS files

**Files:**
- Modify: `crates/anywasm-rt/src/vfs.rs`
- Modify: `crates/anywasm-rt/src/bundle.rs` only if shared path normalization is needed
- Test: VFS unit tests in `crates/anywasm-rt/src/vfs.rs`

**Interfaces:**
- Produces: `Vfs::register_asset_index(bytes: &[u8]) -> Result<(), String>`, `Vfs::file_len(path: &str) -> Option<u64>`, and `Vfs::read_range(path: &str, offset: u64, len: usize) -> Result<Vec<u8>, AssetReadError>`.
- Remote paths and metadata are queryable without reading payload bytes; local files override indexed paths.

- [ ] Write tests for valid indexed metadata, path normalization, directories, file lengths, and malformed/duplicate/out-of-range entries.
- [ ] Run the focused VFS tests and confirm the missing API / validation behavior fails.
- [ ] Implement the `.agp` header/index parser and remote file metadata.
- [ ] Run the focused VFS tests and confirm valid entries pass and invalid indices reject.

### Task 2: Guest read suspension and mapped windows

**Files:**
- Modify: `crates/anywasm-rt/src/hle/kernel32.rs`
- Modify: `crates/anywasm-rt/src/vfs.rs`
- Modify: `crates/anywasm-wasm/src/lib.rs`
- Modify: `scripts/asyncify-wasm.mjs`
- Test: kernel32/VFS tests and a wasm smoke fixture

**Interfaces:**
- Consumes: Task 1 VFS range API.
- Produces: WASM import `anywasm.read_asset_range(package_offset: u64, len: u32, destination: u32) -> u32`; cache miss initiates Asyncify unwind, cache hit copies bytes into the supplied WASM buffer.
- `ReadFile`, file length queries, and file-backed `MapViewOfFile` use indexed metadata and range reads.

- [ ] Add a guest fixture that opens, seeks, and reads a remote file and checks its returned length and bytes.
- [ ] Run the fixture against a no-cache host and confirm it fails because the range import is missing.
- [ ] Add the asyncified range import and ensure read retry advances the handle position once; use the same range path for mapped view windows.
- [ ] Run native runtime tests and the fixture's wasm smoke check.

### Task 3: Browser range cache and worker resumption

**Files:**
- Modify: `web/anywasm.js`
- Modify: `web/worker.js`
- Modify: `web/main.js`
- Modify: `web/serve.mjs`
- Test: `test/wasm_smoke.mjs`, `test/browser/run.mjs`

**Interfaces:**
- Consumes: Task 2 `anywasm.read_asset_range` import and Asyncify module.
- Produces: `AnyWasm.attachAssets(indexBytes, packageUrl)`, `AnyWasm.hostWaitPending`, and `AnyWasm.waitForHost()`.
- Range responses require status `206` and a matching `Content-Range`; concurrent identical chunks share one request; cache capacity is 64 MiB LRU.

- [ ] Add Node/browser coverage for cache miss, cache hit, coalescing, LRU eviction, invalid `206`/`Content-Range`, and rejected full-response fallback.
- [ ] Run those checks and confirm they fail before implementation.
- [ ] Implement index attachment, the 256 KiB range cache, Asyncify wait/resume, loading progress, and HTTP range responses from the local server.
- [ ] Run focused JS checks and existing browser smoke coverage.

### Task 4: GTA package and end-to-end acceptance

**Files:**
- Modify: `scripts/pack-gta-sa.ps1`
- Modify: `web/main.js`
- Modify: `web/worker.js`
- Modify: `docs/BROWSER_SUPPORT.md`, `docs/STATUS.md`, and `CLAUDE.md`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: a small `gta_sa.awb` containing `gta_sa_compact1.0.exe` and required DLLs, plus an indexed `gta_sa.agp` data package.

- [ ] Add packer checks that the selected EXE is in `.awb` and all included non-DLL data is indexed in `.agp`.
- [ ] Run the packer checks against a temporary synthetic game directory and confirm they fail for missing/incorrect outputs.
- [ ] Implement the split packer output and GTA demo companion fetch.
- [ ] Package the installed GTA directory, launch through the browser, and capture requested ranges plus gameplay/input evidence; record any runtime blocker exactly.
- [ ] Run the focused suites and update the checklist only to match observed results.
