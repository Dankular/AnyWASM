# Per-Thread Static TLS Design

**Status:** target-grounded extension to the cooperative Win64 scheduler.

## Target evidence

After the selected D3D11-only `/MT` GLFWDemo passed the thread wait, DXGI, filesystem, and target
CPU checkpoints, the 30,000,000-instruction run crashed at `GLFWDemo.exe+0x96d8` with a read fault
at address `0x4`. The faulting sequence is:

```text
65 48 8B 04 25 58 00 00 00    mov rax, gs:[0x58]   ; TEB.ThreadLocalStoragePointer
48 8B 08                      mov rcx, [rax]       ; TLS vector slot 0 is null
8B 04 0A                      mov eax, [rdx+rcx]   ; rdx=4, fault at address 4
```

The runtime currently starts `tls_next` at 1 and shares that counter between static PE TLS,
`TlsAlloc`, and `FlsAlloc`. The scheduler saves per-thread dynamic TLS/FLS maps but leaves the TEB
static TLS vector pointer shared and does not create separate module TLS blocks for new guest
threads. The trace therefore identifies an actual static TLS gap, not the earlier condition-variable
wait.

## Goal and boundaries

Make static TLS data and its TEB vector slot thread-local for the selected cooperative Win64 guest
threads. Assign PE static TLS indexes from zero; keep Win32 `TlsAlloc` and `FlsAlloc` index spaces
independent from each other and from loader-assigned static TLS indexes. Preserve the current single
TEB address and save/restore its `ThreadLocalStoragePointer` (`TEB+0x58`) as a per-thread field.

This design supports TLS-bearing PE images loaded before guest threads start. A TLS-bearing image
loaded after guest-thread creation remains unsupported and must fail explicitly. It does not add a
unique TEB per thread, parallel execution, APC delivery, or preemptive scheduling.

## Data flow

1. During PE loading, record each module's TLS template bytes, total initialized plus zero-fill size,
   and static index. Initialize the main thread's vector and its block for that module.
2. When a guest thread is created, allocate a new TLS vector and a separate block per recorded PE
   TLS template. Copy initialized bytes and leave zero-fill bytes zero.
3. Store the vector pointer in that guest thread context. On scheduler switches, save and restore it
   through `TEB+0x58` alongside stack bounds and last error.
4. Keep `TlsAlloc` values in the existing per-thread map and `FlsAlloc` values in their separate
   per-thread map, while allocating indexes from independent process-wide counters.

## Failure behavior

- Guest threads use independent static TLS blocks and cannot mutate the main thread's TLS data.
- A TLS-bearing PE module loaded after guest-thread creation fails explicitly rather than leaving
  existing TEB vectors incomplete.
- Non-TLS PE loading, existing dynamic TLS/FLS calls, and the single shared TEB address continue to
  use the current runtime model.

## Verification

1. A freestanding guest PE uses `__declspec(thread)` in main and a worker. The worker must start
   with the image template value, modify its own value, exit, and leave the main thread's value
   unchanged.
2. Scheduler unit coverage verifies distinct vector/block addresses and correct `TEB+0x58`
   restoration across a yield.
3. Verify `TlsAlloc` and `FlsAlloc` index counters are independent and may both start at zero.
4. Run the runtime package regressions and the selected bounded target again; report the next stop
   without claiming game or renderer acceptance.
