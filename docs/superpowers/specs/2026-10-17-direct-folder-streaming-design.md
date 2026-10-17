# Direct Folder Streaming Design

## Goal

Run a Windows executable from a user-selected install folder in the browser without first writing
an `.awb` launch bundle or `.agp` asset package. The runtime loads the selected executable and guest
DLLs as individual files, then streams other files from the selected folder when the guest asks
for them.

The intended benefit is to remove the full-size data copy performed by the pack scripts. File
contents are not converted: guest file reads receive the original bytes. AnyWASM continues to
translate Windows process and file-system behavior at its existing runtime boundary.

## Current project constraints

- `Vfs` stores local files as bytes and indexed files as a path, size, and package offset.
- `ReadFile` and file-backed mapped views already request file-relative ranges from `Vfs`.
- The WASM host suspends a guest call on an uncached range, services it asynchronously, and resumes
  the call through Asyncify.
- The current range host is bound to an HTTP URL. `.agp` payloads are uncompressed concatenations;
  the pack scripts write the EXE and DLLs to `.awb` and copy other files into `.agp`.
- `aw_load` accepts `.awb` bytes and loads the executable before returning. The loader needs the
  executable and statically imported DLL bytes during that call.
- Static DLL resolution can read indexed ranges, but the packer currently puts every DLL in the
  local VFS. `LoadLibrary` also expects local VFS bytes today.
- The page already runs the runtime in a dedicated worker and supports Chromium, Firefox, and
  WebKit to varying degrees.

## Proposed design

### Folder selection and entry point

Add a “Run from folder” flow backed by a directory-enabled file input. The user selects the game or
application root. The page receives a list of `File` objects whose relative paths identify their
location beneath that root, then passes those objects to the runtime worker. The selected file set
is session-scoped; the browser does not upload or write it. The Sims 3 action enters this direct
folder flow and selects `Game\\Bin\\TS3W.exe` when present, instead of looking for a packed demo.

Normalize relative paths using the VFS Windows path rules and reject empty, absolute, traversal,
or duplicate case-insensitive paths. Present the discovered `.exe` paths as the entry-point choice;
when there is exactly one executable, select it automatically. An empty executable list or invalid
file inventory produces a visible launch error.

The directory input is chosen for compatibility with the project’s Chromium, Firefox, and WebKit
browser matrix. It requires a user selection and provides file handles for the selected tree; the
runtime reads those files through `File.slice()` on demand.

### Direct launch ABI

Add staged direct-load exports alongside the existing bundle API:

1. `aw_load_begin` creates the process VFS, records the selected entry path, and registers the
   in-memory asset index before PE dependency resolution.
2. `aw_load_add_file` inserts one boot file into the VFS. The host calls it for the selected EXE and
   every `.dll` under the selected root. Files are copied one at a time into the WASM runtime; no
   aggregate `.awb` bytes are built.
3. `aw_load_finish` loads the EXE, resolves its DLL imports, installs hooks, and makes the process
   ready to run. The direct launch working directory defaults to the selected EXE's parent folder,
   matching normal Windows launch behavior and the existing Sims 3 package's `Game\\Bin` cwd.

All guest DLLs are loaded into the local VFS because current PE startup and `LoadLibrary` paths
expect image bytes to be present synchronously. Other files are represented by the indexed source
and are not read at launch. The existing `.awb` `aw_load` path remains available and unchanged for
bundled demos and installed catalog packages.

### In-memory index and direct range source

Build the existing AGP1 header-and-index shape in memory from the selected file inventory, without
creating an AGP payload. Each non-boot file record contains its normalized relative path, length,
and a synthetic contiguous offset in a virtual address space. This preserves the VFS index parser
and the `read_asset_range(offset, length, destination)` host ABI.

Generalize the browser range cache to read from a source interface. The HTTP implementation keeps
its current range validation and cache behavior. A directory implementation maps a virtual range
to one or more selected files, reads only the required slices, and assembles the requested range.
Direct reads use a bounded byte and entry LRU cache keyed by exact ranges; the initial version does
not coalesce overlapping reads or read ahead. HTTP-backed reads retain their existing chunk
read-ahead behavior. The guest sees the same file lengths, directory membership, read-only asset
behavior, and byte contents as it does with AGP.

The directory source must never read an entire non-boot file to serve a partial range. A virtual
chunk may overlap two adjacent file records, so the provider must assemble it from multiple slices.
Files not represented by the selected inventory, permission/read failures, and short reads must
fail visibly instead of returning fabricated bytes.

### Compatibility and scope

- Keep `.awb` + HTTP `.agp` launch and package-catalog install behavior.
- Keep the pack scripts as an optional static-package publishing path; direct launch does not invoke
  them and does not create package files.
- Direct-folder assets are read-only. Guest writes continue to use the in-memory VFS and are not
  persisted to the selected folder.
- The first implementation supports browser folder selection and the existing Win32/VFS paths. It
  does not add a native CLI directory mount or change CPU, graphics, or Windows API coverage.
- PE entry files and DLL images are read in full because the current loader maps and relocates those
  images. Large data files remain range-read.

## Error handling

- Missing entry point, duplicate normalized paths, unreadable boot files, corrupt in-memory index,
  invalid asset range, short file reads, and disappeared files produce a path-specific runtime
  message.
- The worker reports folder scan, boot-file load, and streamed-byte progress through the current
  load/status UI.
- Browser cancellation leaves the current runtime untouched and reports no launch failure.
- A failed direct source must not silently fall back to downloading or buffering a whole game tree.

## Testing and acceptance

1. JavaScript tests build an index from nested relative paths and reject duplicate or unsafe paths.
2. Direct range tests verify exact bytes for ranges inside one file and across adjacent virtual file
   records. They prove that a partial read calls `slice()` only for the requested spans, that a cache
   hit avoids another file read, and that read failures surface as errors.
3. Rust/C-ABI tests cover the staged loader’s valid sequence, missing entry, duplicate boot path,
   corrupt index, and refusal to run before `aw_load_finish`.
4. A WASM/browser fixture launched from a selected directory reads a data file through the
   Asyncify miss/resume path, prints its expected contents, and exits successfully without an
   `.awb` or `.agp` file.
5. Existing bundle, HTTP range, package-manager, and browser smoke paths continue to pass.
6. A manual local GTA/Sims 3 folder run, when the corresponding user-owned install is available,
   confirms the correct executable selection and records which guest data ranges were read. This is
   an integration check, not a gameplay-compatibility claim.

Performance will be measured after correctness. This design removes the multi-gigabyte packaging
copy but does not claim that direct file reads are faster than the packed HTTP cache.
