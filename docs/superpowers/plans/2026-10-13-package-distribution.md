# AnyWASM Package Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (the owner selected native execution). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a versioned, installable browser package catalog and speed up bounded AGP range streaming.

**Architecture:** Static catalogs reference data-only versioned manifests. A browser package manager validates dependency graphs and artifacts, stores installed AWB bundles in IndexedDB, and launches them with the existing remote AGP streamer. The range cache adds bounded prioritized concurrency and short sequential read-ahead.

**Tech Stack:** Browser JavaScript modules, IndexedDB, Fetch Range, Web Crypto SHA-256, PowerShell package generation.

**Spec:** `docs/superpowers/specs/2026-10-13-package-distribution-design.md`

## Global Constraints

- Keep `gta_sa_compact1.0.exe` as the GTA entry point.
- Keep `.awb` compatibility and the existing AGP1 layout.
- Keep game payloads remote and range-readable; never fetch the full AGP during installation.
- Keep the runtime range cache at or below 64 MiB, with at most four active requests.
- Never add user game binaries to Git.

## Review Focus

- Invalid or hostile catalog URLs and metadata must fail before writes.
- Dependency cycles, missing pins, and conflicting versions must fail before downloads.
- Interrupted installation must not replace a previously installed version.
- Range demand must outrank speculative prefetch and failed speculation must not fail the guest read.
- Browser storage denial or quota errors must surface as install failures without reporting success.

---

### Task 1: Catalog, manifest, dependency resolver, and IndexedDB store

**Files:** Create `web/package-manager.js`; create `web/catalog.json`; add non-test code only.

**Interfaces:** `PackageManager.addCatalog(url)`, `PackageManager.refreshCatalogs()`, `PackageManager.install(id, version)`, `PackageManager.getInstalled(id, version)`, `PackageManager.listInstalled()`, `PackageManager.remove(id, version)`, and `PackageManager.close()`.

- [x] Implement strict catalog/manifest validation, exact-version dependency graph resolution, four-task bounded artifact concurrency, and IndexedDB transactions.
- [x] Run JavaScript syntax checks and install the actual local GTA package; the browser accepted its bundle and AGP index hashes and displayed the installed version.
- [x] Exercise invalid catalog URLs and manifests, missing/cyclic/conflicting dependencies, interrupted replacement downloads, storage-quota failures, and the 128-package graph boundary in a 10-case headless Chromium suite. A valid 128-package closure installs; a larger graph fails before artifact downloads.

### Task 2: Package manager UI and installed launch

**Files:** Modify `web/index.html` and `web/main.js`.

**Interfaces:** Catalog URL input, available-package cards, install/remove/launch actions, installed-state labels.

- [x] Render catalogs and state through DOM text APIs; add catalog and install controls.
- [x] Launch the selected installed bundle from IndexedDB with its manifest entry point and remote asset URL. The bounded browser capture entered the guest and reached 30 presents with no page or GPU errors.
- [x] Verify reload persistence, uninstall, and dependency protection through the shipped page. An installed app and its dependency survive reload; the dependency cannot be removed until the app is removed.

### Task 3: Package generation and cache scheduling

**Files:** Modify `scripts/pack-gta-sa.ps1`, `web/assets.js`, `web/anywasm.js`, and `web/worker.js`.

**Interfaces:** GTA packer writes versioned package metadata; `AssetRangeCache` exposes bounded concurrency and same-file read-ahead.

- [x] Emit a SHA-256 checked AWB manifest and local catalog record while preserving the compact entry point. `-MetadataOnly` validated the existing local AWB/AGP against the installed file index and wrote both sidecars.
- [x] Schedule demand range reads ahead of speculative work, cap active requests at four, and prefetch at most three upcoming chunks.
- [ ] Compare a bounded New Game profile with the recorded baseline. The current run stopped at 653 presents before New Game, so there is no comparable speed result; record this limit in `docs/TARGETS.md`.
