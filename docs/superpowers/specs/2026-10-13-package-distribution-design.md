# AnyWASM Package Distribution Design

## Goal

Provide a CKAN-style static package catalog for browser-run Windows applications. Users can add catalogs, inspect versioned packages and dependencies, install or remove versions, and launch installed applications. The package manager stores the small runtime bundle locally while large AGP data stays on the server and continues to load through HTTP ranges.

## Package model

- A catalog is JSON containing package IDs, versions, and manifest URLs. Catalogs can be hosted as static files and added by URL.
- A package manifest identifies an application, its entry point, a SHA-256 checked AWB bundle, an AGP URL and expected package length, the SHA-256 of the AGP header/index prefix, and exact-version dependencies.
- IDs and versions are validated. Dependency resolution rejects missing versions, cycles, and conflicting versions before downloading artifacts.
- Installs are persisted in IndexedDB. A package's bundle and its dependency records are committed together after all downloads and integrity checks succeed. Removing an install is refused while another installed package depends on it.
- Launch loads the cached bundle and passes the AGP URL to the existing runtime range reader. Installation fetches only the AGP header and index; it never downloads the complete game data package.
- No copyrighted game files are checked in. The local GTA packer writes a package manifest and catalog entry next to the user-supplied AWB/AGP outputs.

## Speed and memory

- Package artifact and index tasks use bounded concurrency of four.
- The runtime keeps a 64 MiB LRU range cache and at most four active range requests. Sequential reads can prefetch up to three upcoming 256 KiB chunks from the same indexed file. Demand reads outrank queued speculative reads.
- Failed speculative reads are ignored until the guest demands those bytes; demand failures remain visible.

## Boundaries

- Catalogs and manifests are data only; they cannot execute code.
- This version installs complete runnable applications and dependency metadata. It does not merge mod overlays into a guest VFS.
- Asset data integrity is enforced by exact HTTP range validation and package length; manifest integrity checks the bundle and AGP index prefix. Per-chunk cryptographic hashes are a later extension.

## Acceptance

1. Catalog/manifest validation rejects malformed IDs, versions, URLs, hashes, duplicate versions, missing dependencies, conflicting versions, and dependency cycles.
2. Install persists bundle bytes only after bundle and AGP-index validation; a failed download leaves prior installs intact.
3. Relaunch uses the installed bundle and remote AGP URL; removing an unused package frees its stored bundle, and required packages cannot be removed.
4. A local GTA package build emits a manifest and catalog record for `gta_sa_compact1.0.exe` without adding game binaries to Git.
5. The range cache bounds active requests, prioritizes guest demand, coalesces duplicate chunk requests, and never exceeds its configured cache budget.
