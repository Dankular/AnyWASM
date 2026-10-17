# Direct Folder Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Launch a Windows program from a user-selected folder while streaming non-boot files directly from browser `File` objects, without producing `.awb` or `.agp` files.

**Architecture:** Build a virtual AGP1-compatible index in memory, map its ranges to selected files, and reuse the bounded asset cache. Add a staged C ABI that registers the index before the EXE and DLL images load. Wire the directory input to the existing worker while preserving bundle and HTTP-backed package launches.

**Tech Stack:** Rust WASM runtime, JavaScript ES modules, WebAssembly Asyncify, Node test runner, browser directory file input.

**Spec:** `docs/superpowers/specs/2026-10-17-direct-folder-streaming-design.md`

## Global Constraints

- Keep `.awb` + HTTP `.agp` launch and package-catalog install behavior.
- Direct-folder assets are read-only. Guest writes continue to use the in-memory VFS and are not persisted to the selected folder.
- The first implementation supports browser folder selection and the existing Win32/VFS paths. It does not add a native CLI directory mount or change CPU, graphics, or Windows API coverage.
- PE entry files and DLL images are read in full because the current loader maps and relocates those images. Large data files remain range-read.
- Direct launches use the selected executable's parent folder as the guest working directory.
- Do not modify or remove the user's existing Sims 3 edits and untracked artifacts.

## Review Focus

- Duplicate normalized file paths: reject the inventory with a useful path error.
- Unsafe relative paths and an empty executable list: reject before starting the worker.
- A requested range crossing file boundaries: assemble exact bytes from only the requested file slices.
- Short or rejected file reads: fail the host read rather than returning zero-filled data.
- A staged load that has not finished: refuse to run the guest.

## Files and interfaces

- Create `web/direct-assets.js` for selected-folder indexing and direct file-range reads.
- Modify `web/assets.js` so its range cache can consume either HTTP ranges or a direct file-range source without changing HTTP defaults.
- Modify `crates/anywasm-wasm/src/lib.rs` to export `aw_load_begin(entry,index)`, `aw_load_add_file(path,data)`, and `aw_load_finish()`.
- Modify `web/anywasm.js` with a `loadDirectory(entry, files, onProgress)` method that drives those exports and attaches the direct range source.
- Modify `web/worker.js`, `web/main.js`, and `web/index.html` to launch the selected directory and preserve the existing bundle start message.
- Add JavaScript tests in `test/direct-assets.test.mjs` and a WASM staged-load smoke test in `test/wasm_direct_folder_smoke.mjs`.

### Task 1: Direct inventory and range source

**Interfaces:** `inspectDirectory(files)` returns `{ filesByPath, executables }`, with paths normalized relative to the selected root. `createDirectoryAssetIndex(files, bootPaths)` returns `{ bytes, packageLength, fileRanges, filesByPath, executables }`. `DirectoryAssetSource(index).readRange(start, length)` returns exactly `length` bytes or rejects. `AssetRangeCache` accepts an optional exact `readRange(start, length)` callback while retaining its HTTP chunk-cache behavior.

- [x] Write tests for path normalization, duplicate and unsafe path rejection, exact single-file and cross-file reads, partial `slice()` spans, and read failure.
- [x] Run `node --test test/direct-assets.test.mjs` and confirm the expected missing-module/API failures.
- [x] Implement the inventory builder and direct source, then generalize the cache chunk loader to consume that source.
- [x] Run the direct-assets tests and `node --test test/assets.test.mjs`.

### Task 2: Staged loader ABI and JS bridge

**Interfaces:** Begin creates a process and registers the index. Add-file inserts a boot image at a normalized path. Finish loads the selected EXE and installs the process state. All ABI failures populate `aw_message_*`; `aw_run` returns a crash status before finish.

- [x] Add a focused ABI smoke test that stages a valid fixture, rejects a corrupt index and duplicate boot path, refuses to run before finish, and runs the finished process.
- [x] Run the smoke test and confirm it fails because the staged exports are absent.
- [x] Implement the three WASM exports and `AnyWasm.loadDirectory`; preload the selected EXE and each DLL individually, leaving other data in the direct range source.
- [x] Build the WASM runtime and run the staged-load smoke test plus existing WASM asset streaming smoke.

### Task 3: Folder selection and worker launch

**Interfaces:** The directory input sends selected `File` objects and an entry path to the worker. Exactly one `.exe` is selected automatically; multiple executables prompt for a choice, except the Sims 3 action prefers `Game\\Bin\\TS3W.exe` when present. The Sims 3 action opens folder selection directly and never requests a packed demo bundle. Boot and streamed-read progress use the existing loading status.

- [x] Add UI/worker tests for selecting a directory inventory, choosing an entry EXE, and dispatching a direct-folder launch.
- [x] Run them to confirm the new UI and worker protocol are missing.
- [x] Wire the folder picker to the worker and keep package/demo/bundle dispatch unchanged.
- [x] Run the JavaScript tests, Rust runtime tests, build both WASM variants, and run the direct-folder WASM smoke test.

## Rulings

- Work in the existing branch and directory because the user explicitly chose to preserve and work alongside uncommitted Sims 3 changes. Cost if wrong: new feature edits may be harder to isolate from those changes.

## Review follow-up

- Fixed folder preflight errors marking the worker terminal before it had stopped. The browser regression failed before the change (`dataset.status === "3"`) and passes after; preflight errors now preserve worker lifecycle state.
- Deferred minor: the direct source cache is bounded but keys exact `(offset, length)` reads and skips read-ahead/coalescing. Sequential or overlapping reads may repeat `File.slice()` work; optimize after measuring real game traces.
- The broader GTA packer test remains environment-blocked on the host's spawned Windows PowerShell 5.1, which lacks `Get-FileHash`; the failure is in `scripts/pack-gta-sa.ps1`, outside this change. The remaining 25-test batch and package-manager browser suite passed.
