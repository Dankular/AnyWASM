# Shared WebAssembly Memory for Parallel Guest Threads

**Status:** feasibility spike complete; revised architecture documented for review. Production
integration remains unimplemented. A `no_std` CPU-state crate and version-1 worker ABI skeleton are
implemented, but the ABI is not yet connected to worker execution or shared guest memory.

## Goal and scope

Run independent Win64 guest threads concurrently in browser workers while preserving one Windows
process address space. Guest threads must observe the same mapped guest memory and synchronization
objects. Each thread must retain its own architectural CPU state, stack, TEB fields, TLS/FLS values,
and instruction accounting.

The existing cooperative scheduler remains the compatibility fallback when shared Wasm memory is
unavailable or the threaded runtime cannot start. The CLI remains cooperative in this project. The
threaded browser path must be opt-in until its browser and target acceptance criteria pass.

This design is limited to the shared-memory and parallel-execution subsystem. It does not claim to
finish general SEH/C++ behavior, PE32, DLL initialization, D3D11 coverage, AVX/BMI, or the Diligent
sample rendering path. Those remain in the parent runtime goal.

## Current implementation evidence

- `web/serve.mjs` already returns `Cross-Origin-Opener-Policy: same-origin` and
  `Cross-Origin-Embedder-Policy: require-corp`. `web/main.js` only displays whether the
  `SharedArrayBuffer` constructor exists; it does not select a threaded runtime.
- `web/worker.js` creates one `AnyWasm` instance and calls its `run` loop on one worker. The browser
  build in `web/build.sh` targets `wasm32-unknown-unknown` without shared-memory or atomic target
  options.
- `crates/anywasm-wasm/src/lib.rs` stores one process in `static mut STATE`; its safety comment
  depends on a single-threaded Wasm instance and no re-entrant host calls.
- Guest memory is not currently the Wasm linear memory. `crates/anywasm-cpu/src/mem.rs` implements a
  48-bit paged address space with Rust-owned page tables and a growing `Vec<u8>` frame store. The
  JIT accesses its own CPU state and translation-cache entries through the Wasm linear memory, while
  guest loads and stores go through the separate `Memory` object.
- `Process` owns the scheduler, guest thread table, handles, VFS, module state, HLE state, virtual
  clock, output buffers, and GPU command stream. Its current ready queue selects one guest CPU at a
  time. Each `GuestThread` already has a separate `Cpu`, stack bounds, TLS/FLS values, last error,
  and wait state.
- `web/anywasm.js` transfers the Wasm and `.awb` `ArrayBuffer`s to the worker. Game data uses the
  existing range-fetch path. SAB is therefore for shared guest execution state, not a replacement
  for those transfer and streaming paths.
- `crates/anywasm-cpu-state` now owns `Cpu` and the instruction decoder without allocator or `std`
  dependencies. The existing `anywasm-cpu` API re-exports both. The shared ABI defines a 128-byte
  bounded-region header, 16-byte sorted page-map entries, and a 64-byte aligned atomic worker slot
  with sequence-matched publish/claim/complete transitions. The CPU context is 784 bytes and
  16-byte aligned. Header validation checks bounds, alignment, region overlap, context offsets, and
  frame mappings; failed worker slots become disabled after their completion is consumed.
- The ABI crate is not loaded by `web/worker.js`; the current runtime still runs cooperatively and
  keeps guest frame bytes in `Memory`'s `Vec<u8>`. CPU instruction execution, the JIT, and guest
  memory access still live in the `std` crate. The worker control protocol has not yet been exercised
  between browser workers.

## Feasibility spike result (2026-10-12)

- A Rust `no_std` module compiled for `wasm32-unknown-unknown` with `+atomics`, imported shared
  memory, and a 1 GiB maximum. Two Node worker threads each instantiated the module against the same
  memory, ran distinct contexts with separate exported stack-pointer values, and overlapped inside
  the Wasm function. The shared atomic counter reached exactly 40,000; the context markers and
  guest-data sentinel remained intact. The exported `__data_end` was 1 MiB, before its
  synchronization and guest-data offsets. Low-boundary stack canaries and expected context
  checksums were preserved.
- A full-runtime-shaped probe proved that separate Wasm instances share mutable Rust static data
  and a stand-in cursor value when they import the same linear memory. It did not exercise or
  establish the behavior of `std`'s allocator.
- Building `anywasm-wasm` with shared-memory linker flags failed before Asyncify. `rust-lld` reported
  that the prebuilt `std` object was not compiled with `atomics` or `bulk-memory`. The installed
  `wasm32-unknown-unknown` standard library therefore cannot be linked as a shared-memory runtime
  with the current toolchain configuration. Rust also warned that `-Ctarget-feature=+atomics` is
  unstable on this toolchain.
- Result: the full-runtime-per-worker layout is blocked. The no-std CPU-worker boundary is viable
  as a prototype, but it does not yet execute the emulator's CPU or share the process's guest page
  backing store.

### Revised candidate architecture

Keep `anywasm-wasm` as the single process coordinator with its current private Wasm memory and
Asyncify stack. Give CPU workers a separate `no_std` Wasm module that imports a bounded shared
`WebAssembly.Memory`, does not allocate or mutate process statics, receives explicit CPU context and
scratch offsets, and has its stack pointer assigned to a disjoint range before dispatch. The probe
demonstrates atomic coordination and independent stack/context regions for this module shape.

The coordinator remains authoritative for page mappings, HLE, handles, module lifecycle, waits,
exceptions, input, output, and GPU commands. The shared worker memory holds guest frame bytes and
published mapping/context snapshots. A host bridge over the shared buffer provides serialized
coordinator reads/writes and mapping publication; workers use the shared frame bytes directly.
Workers return complete contexts at slice boundaries, so the coordinator can service HLE and
publish changed pages before resuming them. Asyncify remains coordinator-only. The worker runtime
starts with the interpreter; JIT integration requires a later design step.

The first boundary now extracts the architectural `Cpu` context and decoder into
`anywasm-cpu-state`; instruction execution and the `Memory` implementation still use `std`, heap
collections, a Rust-owned frame `Vec<u8>`, and generated JIT state. No production CPU worker has
been integrated. The next steps are to extract a no-allocation interpreter core, implement the
atomic shared guest-frame access backend and mapping-generation protocol, then connect the ABI to
browser workers before target replay.

## Success criteria

1. A browser capability check selects the threaded runtime only when the page is cross-origin
   isolated, shared Wasm memory is supported, the memory object can be created with its configured
   maximum, and required startup allocations succeed.
2. Two or more guest CPU workers execute distinct guest threads concurrently against one guest
   address space. A guest thread's general-purpose, SIMD/x87, flags, RIP/RSP, TEB/TLS, and accounting
   state cannot overwrite another thread's state.
3. A guest write by one worker is visible to another worker. Synchronization HLE operations,
   page-map changes, DLL loads, exception dispatch, and other process-wide mutations have a defined
   serialization point.
4. Guest data races and x86 locked operations do not create Rust data races or unsound aliasing in
   the emulator. Memory ordering and atomicity match the supported x86 guest behavior.
5. HLE calls yield to the process coordinator with a complete thread context. The coordinator
   handles the call and returns the updated context or a block/exit/exception result to the owning
   worker.
6. The existing single-worker path continues to run when isolation, shared memory, worker creation,
   or memory reservation fails. Failure to start the threaded path is reported and never leaves a
   partially initialized process running.
7. A browser fixture demonstrates actual overlapping execution and shared-memory synchronization;
   a target replay demonstrates that the selected guest's thread path reaches a later observable
   milestone than the cooperative path. Thread-pool activation alone is not acceptance.

## Architecture

### Separate process coordination from guest CPU execution

Keep one process coordinator responsible for process-wide state: module and DLL lifecycle, VFS,
handle table, heap and virtual-memory operations, HLE dispatch, scheduler policy, exception
dispatch, virtual time, input, output, and GPU command submission. Only the coordinator mutates this
state. It remains the authority for thread creation, waits, signals, guest faults, and process exit.

Create a browser guest-worker pool for CPU execution. A worker owns one guest thread context at a
time and can be reassigned after that thread blocks or exits. It executes a bounded instruction
slice, then returns at the first of: budget expiration, HLE boundary, guest exception/fault, yield,
or thread exit. A run queue and atomic work-state slots in shared memory coordinate worker claims;
the coordinator remains responsible for Windows scheduling semantics and decides which contexts
are eligible to run.

The coordinator and CPU workers share guest-frame bytes through a host bridge and worker memory, but
do not share Rust `Process` or `Memory` objects. Workers receive stable offsets for the selected
thread context, page-table view, HLE trap table, and per-worker scratch area. Guest addresses remain
64-bit PE virtual addresses and are translated through the same sparse mapping model. The
coordinator publishes HLE thunk addresses and a generation whenever DLL load/unload or thunk
creation changes the trap table; workers stop before executing a published HLE thunk.

### Shared-memory ABI and initialization

The worker ABI uses a `WebAssembly.Memory` created with `shared: true` and an explicit maximum.
The shared region contains a versioned header, worker-control slots, thread contexts, guest frame
bytes, mapping/code/dispatch generations, and atomic synchronization words. Region offsets and sizes
are checked before any worker starts. The coordinator keeps the authoritative mapping tree and
publishes a snapshot at mapping-generation changes; workers keep private translation metadata and
TLBs. The feasibility probe found that full-runtime-shaped instances alias mutable statics,
including the stand-in cursor value, and that the installed standard library cannot link as a
shared-memory module. Only the separate no-std
worker-module layout remains a candidate.

The current Rust-owned `Memory` implementation must be split into a process-owned mapping manager
and a shared guest-frame backend. The mapping manager allocates and removes guest pages and changes
permissions. It publishes a mapping generation only after a complete update and a worker-quiescence
barrier. Workers receive a private mapping snapshot and invalidate their per-worker TLB before
resuming. Guest frame bytes live in shared backing. The current `Vec<u8>` frame store cannot remain
the authoritative data store for this mode.

The process coordinator keeps its private linear memory and Asyncify stack. CPU worker modules do
not use an allocator or mutable process statics. Their initialized data stays in a reserved shared
region, and each worker receives a disjoint stack range through its instance-local stack pointer.
The coordinator publishes guest data only after worker modules have initialized. Asyncify is not
applied to worker modules; workers always return to the coordinator at a safe boundary. The current
full-runtime-per-worker layout remains disabled because it shares `static mut STATE` and the probe's
stand-in cursor value across instances.

### Worker-control protocol and budgets

Each worker slot has an atomic state and monotonically increasing sequence number. The coordinator
writes a thread-context offset, bounded instruction budget, and mapping/code/dispatch generations, then
publishes `READY` with release ordering. A worker claims only a matching `READY` sequence with
acquire ordering and changes the slot to `RUNNING`. At a slice boundary it writes back the complete
CPU context, retired count, stop reason, and any HLE/fault payload before publishing one of
`QUANTUM`, `HLE`, `FAULT`, `THREAD_EXIT`, or `FAILED`. The coordinator consumes only completion
records whose sequence matches the active dispatch, applies process-wide effects, and then returns
the slot to `IDLE` or assigns its next context. `STOP` requests are checked between translated
blocks or interpreted instructions; teardown waits for all slots to become quiescent before releasing
shared memory.

The coordinator reserves each worker's instruction budget from the process-wide remaining budget
before dispatch. Workers return the unused reservation with their exact retired count. A process-wide
budget stop prevents further dispatch once no budget remains; parallel workers cannot each spend the
full process budget. Guest TSC and instruction count remain per-thread CPU state, while aggregate
process instruction counts are updated from completed slices. The coordinator alone advances the
virtual wall clock and frame time.

Failure to create the shared memory or worker pool before the first guest instruction selects the
cooperative fallback. A worker failure after it has begun executing fails the process unless the
worker had already published a complete safe-point context; the runtime never guesses which
instructions or guest-memory writes completed.

### Guest memory concurrency

The shared-memory backend must preserve guest-visible x86 semantics while remaining sound Rust.
All concurrent access to shared frame bytes goes through a documented access layer. Aligned scalar
loads/stores use atomic operations at the widths for which the guest architecture guarantees
atomicity. Misaligned and cross-page accesses follow the documented x86 behavior. Locked
read-modify-write instructions use atomic operations or a process-wide memory lock with a documented
linearization point. HLE writes to guest buffers use the same access layer. Page mapping and
protection changes stop all workers at safe points before publication; workers cannot retain stale
translations across a mapping generation change.

Synchronization API calls such as events, mutexes, critical sections, SRW locks, condition
variables, and waits stop CPU execution and go to the coordinator. The coordinator applies their
state transitions in FIFO order, updates the guest thread state and TEB/TLS fields, then schedules
resumption. Address-space topology or protection changes require a generation-based quiescence
barrier: all running workers reach a safe point before the coordinator changes mappings, permissions,
or module code ownership. The coordinator publishes the new mapping generation and per-worker mapping
snapshots before any worker resumes. Writes to executable guest pages increment a separate code
generation; workers invalidate decoded/JIT blocks at their next block boundary. Worker-side host
locks are implementation details and never replace guest Win32 synchronization semantics.

Parallel execution does not promise a deterministic ordering for unsynchronized guest data races.
The synchronized guest fixture must be deterministic; worker completion order and coordinator HLE
events are recorded so a target run can be diagnosed.

### HLE and exception boundaries

Workers stop before executing an HLE thunk's process-state mutation. They report the thread ID,
complete CPU context, thunk identity, and guest arguments to the coordinator. The coordinator
executes the HLE against its process state and returns updated registers, flags, memory writes,
thread state, and next RIP. Blocking HLE leaves the context parked without occupying a worker.

Guest faults and exception-triggering instructions return their faulting context to the coordinator.
Existing `Process` exception dispatch remains the sole authority for SEH/C++ search and unwind.
This preserves deterministic exception ownership and avoids concurrent mutation of module unwind
metadata. Synchronous callbacks, Asyncify host I/O, and GPU readback remain coordinator-owned; a
worker cannot suspend an Asyncify stack owned by another worker.

### JIT and decoded-code state

Each CPU worker has private decoded-block and JIT caches. Generated Wasm blocks receive the shared
guest-memory backing and that worker's context/TLB offsets. Self-modifying writes to executable
pages advance the code generation; `VirtualProtect` and page remapping advance the mapping
generation; DLL load/unload can advance both mapping and dispatch generations. Workers check the
relevant generation before each translated block and discard stale translations before executing
again. JIT compilation and cache eviction do not mutate another worker's private cache.

### Browser capability and fallback

The UI checks `crossOriginIsolated`, `SharedArrayBuffer`, shared `WebAssembly.Memory`, and worker
creation. The server keeps its current COOP/COEP headers. Deployments that cannot provide those
headers use the existing cooperative runtime. A threaded artifact is built separately from the
current artifact until its instruction feature set and deployment requirements are stable. The
browser reports the selected mode and the specific fallback reason.

Shared memory must have a configured maximum because the WebAssembly threads design requires one.
Failure to construct the memory or satisfy startup allocations occurs before guest execution and
selects the cooperative fallback. A later memory-growth/allocation failure follows an explicit
guest allocation failure path and never detaches or replaces the shared buffer.

## Data flow

1. The page detects cross-origin isolation and fetches the threaded Wasm artifact when eligible.
2. The host creates the bounded shared memory and starts the process coordinator and worker pool.
3. The coordinator initializes process state exactly once, loads `.awb`, attaches optional streamed
   assets, and publishes the guest page/frame mappings.
4. The scheduler assigns ready guest contexts to free CPU workers through atomic work slots.
5. Workers execute guest instructions and shared-memory accesses until a defined yield boundary.
6. At an HLE, fault, exception, wait, or exit boundary, the worker publishes its context and status;
   the coordinator performs process-wide effects and schedules the next eligible context.
7. The coordinator alone forwards input, GPU commands, stdout/stderr, frame events, and final status
   to the page.

## Alternatives considered

1. **Keep cooperative guest threads and use SAB for a bundle or message buffer.** Rejected: current
   page-to-worker `ArrayBuffer` transfer already transfers ownership without copying, and game data
   already uses range reads. This would not make guest threads parallel or share guest memory.
2. **Share the current Wasm memory and instantiate the existing full runtime in every worker.**
   Rejected as-is: current module state is a singleton in linear memory, guest memory is a separate
   Rust `Vec`, and repeated module initialization would alias mutable statics; allocator behavior
   was not measured. This would introduce races and possible memory corruption.
3. **Shared guest memory with a coordinator and CPU-worker boundary.** Selected. It makes shared
   guest bytes explicit, keeps Windows process state under one owner, and gives workers independent
   CPU execution state. It requires a feasibility spike and a shared-memory access redesign before
   the full worker pool is implemented.

## Verification requirements

- Wasm build checks confirm the threaded artifact imports/exports shared memory with the configured
  maximum and uses only supported browser features. The existing single-thread artifact remains
  buildable and runnable.
- Worker ABI tests verify region bounds, one-time initialization, worker-local state isolation,
  shared guest reads/writes, sequence-matched handoff, process-wide budget reservation,
  mapping-generation invalidation, and worker failure cleanup.
- CPU memory tests cover concurrent aligned loads/stores, misaligned and cross-page accesses,
  HLE guest-buffer writes, locked RMW instructions, self-modifying code, protection changes, and
  unmapping while other contexts are parked. Address-space changes prove the quiescence barrier
  waits for every running worker and publishes one new generation.
- A guest fixture starts at least two threads, uses Win32 synchronization and an interlocked shared
  counter, checks per-thread TLS/TEB isolation, joins both threads, and verifies a deterministic
  final result across repeated runs.
- Browser tests cover shared-mode selection under cross-origin isolation, fallback without
  isolation, worker startup failure, configured memory-limit failure, stop/dispose during work, and
  normal input/GPU/output delivery.
- Target acceptance uses a bounded Diligent replay and records worker concurrency, reached imports,
  shader/present counters, and browser pixels separately. A passing synthetic thread fixture does
  not prove Diligent rendering.

## Risks and limits

- The existing Rust Wasm toolchain may not support a safe per-worker static/allocator layout with
  the current `wasm32-unknown-unknown`, Asyncify, and JIT pipeline. The required spike is a hard
  gate, not permission to fall back to a message-only SAB feature.
- Shared guest memory changes the correctness model for ordinary guest stores, atomic instructions,
  code invalidation, and Rust aliasing. Incorrect synchronization can corrupt the emulated process.
- Process-wide HLE remains serialized. This limits parallel speedup for syscall-heavy targets but
  keeps Windows handle and scheduler semantics owned in one place.
- Threaded Wasm requires cross-origin isolation and a fixed memory maximum; some hosts and browsers
  will use cooperative mode or report a specific startup limitation.
- This work does not add Windows preemption, APC delivery, arbitrary nested synchronous callback
  scheduling, or general support for all guest thread APIs.

## References

- [WebAssembly threads proposal](https://github.com/WebAssembly/threads/blob/main/proposals/threads/Overview.md)
- [Rust WebAssembly atomics documentation](https://doc.rust-lang.org/core/arch/wasm32/index.html)
- [SharedArrayBuffer cross-origin isolation requirements](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/SharedArrayBuffer)
