# Game Content Streaming Design

## Goal

Launch the existing `gta_sa_compact1.0.exe` from a small `.awb` while keeping the game files in a separate range-readable data package. The browser fetches only byte ranges requested by the guest, so startup and resident memory do not require reading the full 1.5 GB package.

## Package format

The companion `.agp` package is little-endian: `AGP1`, `u32 entry_count`, `u64 index_length`, `u64 package_length`, then `entry_count` records and the raw file data. Each record is `u16 path_length`, UTF-8 path, `u64 absolute_package_offset`, `u64 file_length`. The packer sorts records by normalized path; payloads are uncompressed. The browser first fetches bytes `0-23`, then fetches the index bytes, validates all offsets against `package_length`, and gives that header plus index to the runtime. File bytes are fetched by HTTP `Range` requests. The packer keeps the selected EXE and guest DLLs in `.awb` and places other files in `.agp`.

## Runtime behavior

The VFS exposes indexed paths, directory membership, and file lengths without loading their contents. Guest `ReadFile` maps its file offset to an `.agp` byte range. A browser host import serves cached bytes synchronously. On a cache miss it queues the aligned 256 KiB chunk, starts Asyncify unwind, and returns control to the worker. The worker fetches and validates `206 Partial Content` plus the exact `Content-Range`, then resumes the same guest call; the retried import copies the now-cached bytes. Failed or ignored ranges produce a visible error and never fall back to downloading the whole package.

The browser retains at most 64 MiB of fetched chunks with LRU eviction. A single guest read may span no more than the cache can retain; larger reads fail with `ERROR_NOT_ENOUGH_MEMORY` instead of repeatedly fetching evicted chunks. Local `.awb` files continue to work without an `.agp`; the GTA demo supplies the companion URL. HTTP range support is required for streaming.

## Compatibility and boundaries

- Existing `.awb` format and behavior remain supported.
- `.agp` stores raw bytes so package offsets map directly to HTTP ranges.
- Remote entries participate in VFS existence, directory, enumeration, and file-size queries.
- `ReadFile` and file-backed mapped views use range reads. A mapped view fetches only its requested window.
- Writes to new paths continue to use the local VFS. An existing indexed asset is read-only unless it is explicitly replaced by a local VFS file.
- The local development server streams requested ranges from disk and rejects malformed or unsatisfiable ranges.
- The GTA packer continues to select `gta_sa_compact1.0.exe` as the entry point.

## Acceptance

1. Unit coverage rejects malformed index lengths, invalid UTF-8 paths, duplicate normalized paths, and offsets outside the data region.
2. A browser test proves a cache miss suspends a guest `ReadFile`, performs a range request, resumes the same call, and returns the exact bytes without advancing the file pointer twice.
3. A second read served from cache performs no network request; cache eviction triggers a later valid range request.
4. A range server test verifies exact `206`, `Content-Range`, and byte content, and rejects ignored/invalid range responses without requesting the whole file.
5. The GTA packer produces the selected EXE in `.awb`, indexes all included content in `.agp`, and the running browser demo reports requested package ranges. This acceptance does not claim the game is playable until it reaches gameplay and accepts input.
