# GTA CPU Acceleration Design

## Objective

Enable useful WebAssembly block JIT execution and parallel guest CPU execution for the GTA San Andreas browser build. The requested product target is 10x less CPU loading time and 10x higher CPU-limited game throughput than the same-build cooperative, cached-interpreter baseline. The target remains open until measurements meet both thresholds.

## Current evidence

- The browser JIT default is `jitMinPrefix = 65`; x86 blocks contain at most 64 instructions, so default GTA execution compiles no blocks.
- A 2026-10-13 `GTA_JIT_MIN_PREFIX=1` browser probe reported 268,068 compilations, 266,175 executions, 414,849 emitted guest instructions, 12,630 fallbacks, and `lastFailure: null` after 1,804,686 guest instructions. It did not create a visible game window within the harness's 60-second startup wait. This disproves the assumption that an emitted-module compile exception is the current GTA blocker; compiling tiny prefixes is also too costly for this startup path.
- A normal-threshold replay reached 653 presents and startup phase 5. JIT counters stayed at zero; the shared mode started two workers but recorded zero parallel batches. The replay did not pass the harness's 990-present menu checkpoint, so it is not a loader or gameplay performance result.
- The cooperative baseline ran 90 seconds after New Game, reached startup phase 8 and 1,080 presents, retired 439,169,995 guest instructions, and still displayed GTA loading artwork. It had no pending asset range and no WebGPU errors; JIT counters remained zero.
- The shared CPU worker path exists, but existing target evidence establishes worker startup and serial context handoff, not overlapping GTA execution. At the measured loader checkpoint only one guest thread was runnable.
- `docs/TARGETS.md` identifies statically linked `vsscanf` parsing as a repeated loader hot path. Disassembly of the installed scanner at `0x8290d4` includes the repeated `MOVZX EBX, byte ptr [ESI]` / `MOVZX EAX, BL` pair at `0x82919c`/`0x82919f`; package range concurrency cannot accelerate periods when the guest has no pending range request. The emitter now generates that pair, but it still lacks x86-32 runtime differential coverage.

## Requirements

1. JIT compilation and execution are enabled for eligible hot GTA blocks by default only after the targeted parser path has exact interpreter parity and measurements show compilation amortizes its cost. Unsupported instructions and invalidated code resume through the interpreter without changing guest behavior.
2. JIT emitted execution matches the interpreter's complete CPU state, mapped guest memory, exceptions, and retired instruction count for targeted 32-bit instruction sequences and the GTA-observed parser path.
3. Browser CPU workers execute multiple independently runnable guest threads at the same time against shared guest memory. The coordinator remains the sole owner of process-wide HLE, scheduler, module, exception, and GPU state. Guest synchronization and locked operations must preserve x86-visible behavior.
4. GTA profiling reports execution mode, worker overlap, JIT compile/execute/fallback counts and causes, retired instructions, presents, range activity, elapsed time, and loader/gameplay phase.
5. On the same GTA binary, data package, browser, and input sequence, loading time to the first stable gameplay frame is at most one tenth of the cooperative baseline. During CPU-limited gameplay, guest throughput or completed frames per second is at least ten times the same-build baseline. If GTA does not reach either measurement point, that requirement remains unverified.

## Design

Keep JIT failure causes observable, then extend the emitted instruction set around decoded, repeatedly executed GTA code. No GTA compile exception was observed in the current forced-emission probe. Keep exact interpreter fallbacks for unsupported instructions, memory faults, HLE boundaries, and code writes. Choose hotness and prefix thresholds from GTA and representative-fixture measurements so successful compilation amortizes its cost.

Run the coordinator in the existing worker and dispatch independent runnable guest thread contexts to separate CPU workers only at safe boundaries. Keep shared guest frames and synchronization atomic, and serialize process-wide changes through the coordinator. Preserve the cooperative path for browser isolation failures and guest schedules with only one runnable thread. Do not count worker startup or a serial handoff as parallel execution.

Use a repeatable Chrome harness for the installed GTA package. The harness records the New Game click time, first gameplay frame, CPU throughput, frame throughput, JIT stats, worker overlap, asset range activity, and errors. Compare fresh cooperative and optimized runs; retain the 10x target as the acceptance threshold rather than treating a fixture win as target acceptance.

## Constraints and risks

- GTA's loader is largely a single guest thread, so guest-thread parallelism alone cannot make its parser 10x faster. The JIT must accelerate that serial path.
- The existing emitter regresses several generic benchmarks. Enabling emission globally without workload evidence could make ordinary programs slower.
- Running Windows guest threads concurrently increases shared-memory and scheduling risk. Locking, waits, faults, HLE calls, and instruction accounting need explicit serialization rules and target-level evidence.
- Reaching a 10x improvement is an empirical requirement, not guaranteed by enabling workers or reporting a nonzero JIT counter.
