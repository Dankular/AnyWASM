# MSVC C++ Exception Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run x64 MSVC C++ throws and catches from PE images without requiring MSVCP/VCRUNTIME DLLs for the C++ personality path.

**Architecture:** Add `msvc_eh.rs` for validating EH3/EH4 metadata, matching catch types, and constructing catch objects. Register the HLE `__CxxFrameHandler3/4` and `_CxxThrowException` entry points, then route search and target unwind through the existing `exception.rs` dispatcher and `GuestUnwind` mechanism. Validate behavior with synthetic metadata and MSVC-generated `/MD` fixtures.

**Tech Stack:** Rust, current PE/SEH runtime, Windows x64 MSVC 14.44 fixture compiler, PowerShell.

**Spec:** `docs/superpowers/specs/2026-10-12-msvc-cpp-exception-support-design.md`

## Global Constraints

- C++ exceptions use the existing process exception dispatcher and guest unwinder.
- Every relative offset, table count, string, object range, and state index is validated before guest memory access.
- Unknown EH versions or unsupported catch features stop with a precise diagnostic.
- Guest catch/copy/destructor code runs as guest code; guest faults re-enter normal exception dispatch.
- The implementation covers x64 PE32+ only; PE32 C++ EH remains in the wider objective.

## Review Focus

- EH4 compressed integers reject truncated, overlong, and overflowing encodings.
- EH3/EH4 offsets cannot escape the owning loaded PE image or point into HLE modules.
- Catch matching observes compiler order, type qualifiers, base adjustments, and catch-all semantics.
- By-value catches use the supplied copy function; by-reference catches preserve aliasing and adjusted address.
- Unwind funclets run exactly once in reverse construction order, including when the catch object copy throws.

---

### Task 1: Add compiler-generated guest fixtures

**Files:**
- Create: `test/fixtures/msvc_eh/exception_fixture.cpp`
- Create: `test/fixtures/msvc_eh/build.ps1`
- Create: `test/fixtures/msvc_eh/README.md`

**Interfaces:**
- Produces: `target/msvc-cpp-eh/eh4-dynamic.exe` built with `/MD /EHsc` and current FH4 metadata.
- Produces: `target/msvc-cpp-eh/eh3-dynamic.exe` built with `/MD /EHsc /d2FH4-` and legacy FH3 metadata.
- Fixture exit values distinguish typed catch, base catch, catch-all, nested catch, and destructor-count failures.

- [x] **Step 1: Add fixture source** that throws a trivially copyable derived exception, catches it by value, catches its base by reference, catches a second exception with `catch (...)`, and verifies local destructor execution.
- [x] **Step 2: Add build script** that initializes the installed VS x64 environment, builds both variants to the ignored target directory, and reports the compiler/linker versions.
- [x] **Step 3: Build and inspect imports** with the repository analyzer; verify the dynamic fixtures import `_CxxThrowException` and the expected frame-handler version without bundling runtime DLLs.

### Task 2: Parse EH3 function metadata safely

**Files:**
- Create: `crates/anywasm-rt/src/msvc_eh.rs`
- Modify: `crates/anywasm-rt/src/lib.rs`
- Test: `crates/anywasm-rt/src/msvc_eh.rs`

**Interfaces:**
- `pub(crate) fn frame_handler3(p: &mut Process) -> Result<u64, Stop>` handles the x64 HLE ABI.
- `parse_func_info3(p: &Process, image_base: u64, handler_data: u64) -> Result<FuncInfo3, Stop>` returns validated relative EH3 tables.
- `parse_throw_info(p: &Process, image_base: u64, throw_info: u64) -> Result<ThrowInfo, Stop>` returns the throw object and catchable-type records.

- [x] **Step 1: Write failing parser tests** for a valid EH3 FuncInfo, a valid ThrowInfo/CatchableType list, out-of-image RVAs, overflowing counts, and malformed TypeDescriptor names.
- [x] **Step 2: Run tests** with `cargo test -p anywasm-rt msvc_eh::tests::parse_func_info3` and confirm valid inputs fail as not implemented while malformed inputs fail explicitly.
- [x] **Step 3: Implement EH3 readers** using the 32-bit image-relative fields and supported magic values `0x19930520` through `0x19930522`; cap all table counts.
- [x] **Step 4: Run the parser tests** and verify every malformed case returns a diagnostic without reading outside guest memory.

### Task 3: Decode EH4 maps safely

**Files:**
- Modify: `crates/anywasm-rt/src/msvc_eh.rs`
- Test: `crates/anywasm-rt/src/msvc_eh.rs`

**Interfaces:**
- `decode_eh4_uint(p: &Process, cursor: &mut u64, end: u64) -> Result<u32, Stop>` decodes one bounded compressed integer.
- `parse_func_info4(p: &Process, image_base: u64, handler_data: u64) -> Result<FuncInfo4, Stop>` returns normalized EH4 try, handler, IP-state, and unwind maps.
- `pub(crate) fn frame_handler4(p: &mut Process) -> Result<u64, Stop>` handles the x64 HLE ABI.

- [x] **Step 1: Write failing decoder tests** for one-byte, multi-byte, maximum valid, truncated, overlong, and overflow encodings.
- [x] **Step 2: Write failing EH4 metadata tests** for optional header fields, try/handler maps, IP-to-state maps, and unwind maps.
- [x] **Step 3: Implement the bounded decoder and EH4 normalizer** following the installed MSVC `ehdata4_export.h` format; reject reserved headers and invalid RVAs.
- [x] **Step 4: Run the focused parser tests** and verify normalized EH4 records match fixture metadata.

### Task 4: Match thrown types and construct catch objects

**Files:**
- Modify: `crates/anywasm-rt/src/msvc_eh.rs`
- Test: `crates/anywasm-rt/src/msvc_eh.rs`

**Interfaces:**
- `match_handler(p: &Process, exception: &CppException, handler: &HandlerType) -> Result<Option<CatchMatch>, Stop>` checks catchable type names, qualifiers, ellipsis, and PMD adjustment.
- `build_catch_object(p: &mut Process, frame: u64, exception: &CppException, matched: &CatchMatch) -> Result<u64, Stop>` returns the guest catch-object address.

- [x] **Step 1: Write failing tests** for exact type match, base catch, catch ordering, catch-all, incompatible qualifiers, by-reference aliasing, and virtual-base metadata rejection.
- [x] **Step 2: Add catch-object tests** for adjusted base pointers, simple by-value copy, guest copy-function invocation, and invalid frame offsets.
- [x] **Step 3: Implement type matching and object construction** from CatchableType and HandlerType records; keep unsupported virtual-base conversions explicit.
- [x] **Step 4: Run matching tests** and verify exception records are unchanged when no handler matches.

### Task 5: Integrate C++ search and target unwind

**Files:**
- Modify: `crates/anywasm-rt/src/msvc_eh.rs`
- Modify: `crates/anywasm-rt/src/exception.rs`
- Modify: `crates/anywasm-rt/src/hle/crt.rs`
- Test: `crates/anywasm-rt/src/msvc_eh.rs`

**Interfaces:**
- `frame_handler3` and `frame_handler4` use `(exception_record, establisher, context_record, dispatcher_context)` from the existing x64 personality ABI.
- A selected catch returns through `GuestUnwind` with the catch funclet address, establisher frame, exception record, and updated context record.
- `_CxxThrowException` maps `(object, throw_info)` to the existing `dispatch_raised_exception` path with the exception image base.

- [x] **Step 1: Add dispatcher tests** that invoke `__CxxFrameHandler3/4` through `Process::call_guest` for typed search, catch-all search, no-match continuation, and target unwind.
- [x] **Step 2: Implement handler registration** in `crt::lookup` and route `_CxxThrowException` through `exception::dispatch_raised_exception`.
- [x] **Step 3: Implement active-state search** from the EH3/EH4 IP-to-state map and choose the first compatible handler in the selected try block.
- [x] **Step 4: Implement state unwind actions** through guest funclets, then install the selected catch object and transfer through `GuestUnwind`.
- [x] **Step 5: Run dispatcher tests** and verify catch returns, destructor markers, noncontinuable flags, and original exception propagation.

### Task 6: Run the MSVC fixtures without runtime DLLs

**Files:**
- Modify: `test/fixtures/msvc_eh/README.md`
- Modify: `docs/TARGETS.md`

- [x] **Step 1: Build both fixture variants** with `test/fixtures/msvc_eh/build.ps1`.
- [x] **Step 2: Run each fixture with an HLE-only VFS** and a bounded instruction cap; require the expected guest exit code and no unresolved C++ runtime import stop.
- [x] **Step 3: Run each fixture with the installed MSVC/UCRT DLLs present** and require identical guest results.
- [x] **Step 4: Record exact compiler flags, import lists, trace stops, and return values** in the fixture README and `docs/STATUS.md`.

### Task 7: Verify runtime and workspace integration

**Files:**
- Review: `crates/anywasm-rt/src/msvc_eh.rs`
- Review: `crates/anywasm-rt/src/exception.rs`
- Review: `crates/anywasm-rt/src/hle/crt.rs`
- Review: `docs/STATUS.md`

- [x] **Step 1: Run `cargo test -p anywasm-rt`** and verify all exception and CRT regressions pass.
- [x] **Step 2: Run `cargo build --workspace`** and verify every crate builds.
- [x] **Step 3: Re-run Tutorial03 HLE-only and runtime-DLL browser captures**; verify a visible frame, the import report, and WebGPU error count.
- [x] **Step 4: Update status notes** to distinguish EH3/EH4 coverage, actual Diligent rendering, and remaining PE32/C++ gaps.
