# Cooperative guest-thread scheduler design

**Status:** design for implementation against the selected Diligent GLFW Demo target.

## Verified target evidence

The current D3D11-only `/MT` GLFW Demo build is the representative target. A fresh 30,000,000-
instruction AnyWASM trace reaches `kernel32!SleepConditionVariableSRW` after window-creation API
calls. The captured wait has condition address `0x300048298`, SRW-lock address `0x300048228`,
`dwMilliseconds=INFINITE`, flags `0`, thread ID `4104`, and `thread_call_depth=1`. Its thread handle
has no exit code. The runtime currently runs `CreateThread` callbacks synchronously through
`Process::call_guest_thread`; the callback therefore occupies the host call stack until it returns.
The trace ends before any D3D11 renderer or real-game behavior is verified.

The current HLE tables already expose thread handles, manual and auto-reset events, TLS/FLS,
critical-section and SRW-lock imports, condition-variable imports, `Sleep`, and wait imports.
Several synchronization operations are currently success-returning placeholders; `WaitForSingleObject`
polls instead of blocking, and an infinite condition-variable wait stops as unimplemented.

## Goal and success criteria

Implement deterministic, resumable Win64 guest threads in one host process so a blocked callback
can yield its guest CPU state, let its parent or another guest thread run, and resume after a
matching wake or timeout. Preserve fail-loudly behavior for APIs that remain unsupported.

The target-grounded acceptance checkpoint is that the exact infinite condition-variable call no
longer traps or hangs: the waiting thread is retained, runnable guest work can proceed, and a wake
or a diagnosed no-runnable-thread state is reported. A subsequent bounded target trace must record
the next reached import or stop. This scheduler milestone does not claim a rendered frame or a
real-game pass.

## Architecture

- Use a deterministic cooperative scheduler. Guest threads share one `Memory`, module set, VFS,
  heap, handles, and HLE state. The host never runs guest threads concurrently.
- Store a full `Cpu` snapshot per guest thread, including GPRs, flags, SIMD/x87 state, RIP, RSP,
  TSC, and the thread's retired-instruction count. Store its private stack range, thread state,
  last-error value, and TLS/FLS maps with that snapshot.
- Keep a FIFO ready queue. `CreateThread` adds a runnable context and yields after the HLE thunk
  returns. Blocking waits move the current context to a wait queue. Wake operations move eligible
  contexts back to the ready queue. `ExitThread` records the exit code and wakes threads waiting on
  that thread handle. `ResumeThread` makes a suspended context runnable.
- Yield at synchronization HLE calls and `SwitchToThread`; also rotate after a deterministic
  10,000-instruction quantum so a guest thread that never calls an HLE function cannot monopolize
  execution. The run budget is charged across all guest threads.
- Model thread handles, events, mutexes, and semaphores as signaled/counting objects. Implement
  wait-any and wait-all for supported handles. Implement recursive critical sections and exclusive
  and shared SRW ownership keyed by the guest structure address.
- A condition-variable wait atomically releases its associated lock, blocks the caller, and
  reacquires the same lock before returning to guest code. Wake-one selects the oldest eligible
  waiter; wake-all makes all waiters eligible. A timeout returns `FALSE` and sets
  `ERROR_TIMEOUT` after reacquiring the lock.
- Use a process-wide deterministic virtual clock for deadlines and public time APIs. `Sleep(0)`
  yields; finite sleeps and waits advance the virtual clock when no thread is runnable. Infinite
  waits with no runnable guest thread return the runtime's idle state while retaining all contexts;
  they never spin or discard the blocked thread.
- Preserve the current shared TEB allocation and swap its per-thread fields: thread ID, stack
  base/limit, last error, and `ThreadLocalStoragePointer`. Static PE TLS vectors and blocks are
  private to each guest thread and initialized from the module template. `TlsAlloc` and `FlsAlloc`
  use separate index counters and per-thread maps. TLS-bearing images loaded after guest-thread
  creation fail explicitly. A future unique TEB address per thread is outside this milestone.
- `Process::call_guest` remains a synchronous host callback boundary. Scheduling from inside such a
  nested callback is rejected explicitly until that callback continuation can be represented in
  guest state; it must not be reported as a completed wait.

## Wait and lock behavior

Immediate object availability is checked before timeout handling. A zero timeout returns
`WAIT_TIMEOUT` without blocking when no object is signaled. A finite timeout blocks only while
other guest work can run and is completed at its virtual deadline. Infinite waits remain blocked
until a wake condition becomes true. Auto-reset events and semaphores consume one signal/count for
one waiter; manual-reset events remain signaled until reset. Wait-all consumes all acquired objects
only when every requested object is available.

Critical sections are recursive for their owner. SRW locks track shared readers and one exclusive
owner and are not recursive. A condition-variable wait releases one level of the associated lock
and reacquires it before the wait call returns. Unsupported lock modes or invalid handles return
documented failure values and set a meaningful last error rather than claiming success.

## Verification

1. Add a freestanding guest regression where one worker blocks on a condition variable, another
   worker wakes it, and the main thread joins both. Assert thread IDs, isolated TLS, return values,
   lock ownership after wake, and final exit status.
2. Cover timeout-zero, finite timeout, manual/auto-reset events, semaphore counts, wait-any/all,
   recursive critical sections, contended critical sections, SRW shared/exclusive behavior,
   `Sleep`, `SwitchToThread`, and suspended/resumed threads.
3. Run `cargo test --workspace` and `node test/wasm_smoke.mjs` with the scheduler fixture included.
4. Rerun the selected GLFW Demo with the same D3D11-only `/MT` build and a bounded instruction
   budget. Record the first post-wait blocker and whether any host window, D3D11 device, renderer,
   or frame was actually observed.

## Risks and limits

The selected trace enters the wait from a `CreateThread` callback, so thread creation must stop
being a synchronous host-stack call. A process-wide virtual clock and instruction budget replace
the current active-CPU-only accounting when multiple CPU snapshots run. This model is cooperative,
not Windows preemptive scheduling; true parallel execution, APC delivery, alertable waits, unique
TEBs, and waits initiated inside synchronous host callbacks remain unverified unless the target
forces them into scope.
