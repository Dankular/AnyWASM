//! Process state, PE64 loader and the run loop.

use crate::hle;
use crate::vfs::Vfs;
use anywasm_cpu::cpu::*;
use anywasm_cpu::mem::{PROT_R, PROT_RW, PROT_RWX, PROT_X};
use anywasm_cpu::{BlockCache, Cpu, DecodeCache, Exit, Memory};
use anywasm_pe::{canonical_dll, ImportName, Pe};
use std::collections::{BTreeMap, HashMap, VecDeque};

pub const THUNK_BASE: u64 = 0x7ffe_0000_0000;
pub const THUNK_BYTES: u64 = 0x10_0000;
/// Returning here ends a nested `call_guest`.
pub const RET_MAGIC: u64 = THUNK_BASE + THUNK_BYTES - 0x10;
/// Entry-point return lands here and exits the process with RAX.
pub const EXIT_MAGIC: u64 = THUNK_BASE + THUNK_BYTES - 0x20;
/// Return address installed on a newly created guest thread's stack.
pub const THREAD_RETURN_MAGIC: u64 = THUNK_BASE + THUNK_BYTES - 0x30;

pub const PEB_ADDR: u64 = 0x7ffd_0000_0000;
pub const TEB_ADDR: u64 = 0x7ffd_0001_0000;
pub const PARAMS_ADDR: u64 = 0x7ffd_0002_0000;
pub const LDR_ADDR: u64 = 0x7ffd_0003_0000;
pub const STACK_TOP: u64 = 0x7ff0_0010_0000;
pub const HLE_MODULE_BASE: u64 = 0x7ffc_0000_0000;

/// Why `Process::run` returned.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// The guest called ExitProcess / returned from its entry point.
    Exit(u32),
    /// The guest called an import that has no implementation.
    Unimplemented { dll: String, name: String },
    /// Unrecoverable guest fault or unsupported instruction.
    Crash(String),
    /// The step budget was exhausted; call `run` again to continue.
    StepLimit,
    /// A frame was presented; the host should execute the GPU stream, then call `run` again.
    Frame,
    /// The guest is blocked waiting for a window message; push input, then call `run` again.
    Idle,
    /// Internal control transfer used to return from an explicitly created guest thread.
    ThreadExit(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ThreadWait {
    Handle {
        handle: u64,
        deadline: Option<u64>,
    },
    Multiple {
        handles: Vec<u64>,
        wait_all: bool,
        deadline: Option<u64>,
    },
    Sleep {
        deadline: u64,
    },
    CriticalAcquire {
        address: u64,
    },
    SrwAcquire {
        address: u64,
        exclusive: bool,
    },
    Condition {
        condition: u64,
        lock: u64,
        critical: bool,
        exclusive: bool,
        deadline: Option<u64>,
        outcome: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ThreadState {
    Ready,
    Running,
    Suspended,
    Blocked(ThreadWait),
    Exited(u32),
}

pub struct GuestThread {
    pub cpu: Cpu,
    pub stack_base: u64,
    pub stack_limit: u64,
    pub teb_tls_vector: u64,
    pub last_error: u32,
    pub tls: HashMap<u32, u64>,
    pub fls: HashMap<u32, u64>,
    pub state: ThreadState,
    pub is_guest: bool,
    pub wait_order: u64,
}

#[derive(Clone, Debug)]
struct StaticTlsTemplate {
    index: u32,
    data: Vec<u8>,
    total_size: u64,
}

#[derive(Default)]
pub struct SrwLockState {
    pub exclusive_owner: Option<u32>,
    pub shared_owners: HashMap<u32, u32>,
    pub waiters: VecDeque<u32>,
}

#[derive(Default)]
pub struct CriticalSectionState {
    pub owner: Option<u32>,
    pub recursion: u32,
    pub waiters: VecDeque<u32>,
}

pub type RunResult = Stop;
pub type HleFn = fn(&mut Process) -> Result<u64, Stop>;

#[derive(Clone)]
pub struct Module {
    pub name: String,
    pub path: String,
    pub base: u64,
    pub size: u64,
    pub entry: u64,
    pub is_dll: bool,
    pub exports: HashMap<String, u64>,
    pub ordinals: HashMap<u16, u64>,
    /// PE32+ exception directory and its validated, sorted `.pdata` entries.
    pub exception_table_rva: u32,
    pub runtime_functions: Vec<anywasm_pe::RuntimeFunction>,
    /// Provided by the host (reimplemented library); `group` selects the function table.
    pub hle_group: Option<&'static str>,
}

pub struct Thunk {
    pub group: &'static str,
    pub name: String,
    pub func: Option<HleFn>,
}

/// Win32 handle table entries.
pub enum Handle {
    Std(u32),
    File {
        path: String,
        pos: usize,
        write: bool,
    },
    Event {
        signaled: bool,
        manual: bool,
    },
    Mutex {
        owner: Option<u32>,
        recursion: u32,
    },
    Semaphore {
        count: u32,
        max_count: u32,
    },
    Thread {
        id: u32,
        start: u64,
        parameter: u64,
        exit_code: Option<u32>,
        suspended: bool,
    },
    Module(u64),
    Other,
}

pub struct Process {
    pub cpu: Cpu,
    pub mem: Memory,
    pub vfs: Vfs,
    pub modules: Vec<Module>,
    pub thunks: Vec<Thunk>,
    pub thunk_index: HashMap<(&'static str, String), u64>,
    pub unimplemented_log: Vec<(String, String)>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdin: VecDeque<u8>,
    pub handles: HashMap<u64, Handle>,
    pub next_handle: u64,
    pub heap: crate::hle::heap::Heap,
    pub vmem: BTreeMap<u64, (u64, u8)>,
    pub vmem_next: u64,
    pub cmdline: String,
    pub image_path: String,
    pub current_directory: String,
    pub env: Vec<(String, String)>,
    pub tls_next: u32,
    pub fls_next: u32,
    static_tls_next: u32,
    static_tls_templates: Vec<StaticTlsTemplate>,
    pub fls: HashMap<u32, u64>,
    pub tls_slots: HashMap<u32, u64>,
    pub current_thread_id: u32,
    pub next_thread_id: u32,
    pub current_thread_is_guest: bool,
    pub current_thread_state: ThreadState,
    pub current_wait_order: u64,
    pub current_thread_saved: bool,
    pub guest_threads: HashMap<u32, GuestThread>,
    pub ready_threads: VecDeque<u32>,
    pub schedule_requested: bool,
    pub wait_sequence: u64,
    pub quantum_used: u64,
    pub retired_instructions: u64,
    pub srw_locks: HashMap<u64, SrwLockState>,
    pub critical_sections: HashMap<u64, CriticalSectionState>,
    pub atexit: Vec<u64>,
    pub tick_ms: u64,
    pub qpc: u64,
    pub exit_code: Option<u32>,
    pub rng: u64,
    pub trace_imports: bool,
    pub nested: u32,
    /// Set by an HLE function that has rewritten RSP/RIP itself (longjmp): skip the normal return.
    pub skip_ret: bool,
    /// Set by an HLE function to make `run` return after the current call completes.
    pub yield_request: Option<Stop>,
    pub dcache: DecodeCache,
    pub block_cache: BlockCache,
    pub extra: hle::Extra,
}

fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

impl Process {
    pub fn new(vfs: Vfs) -> Self {
        let mut mem = Memory::new();
        mem.map(THUNK_BASE, THUNK_BYTES, PROT_RWX);
        // Fill thunk space with INT3 so a stray jump into it is caught.
        let fill = vec![0xccu8; THUNK_BYTES as usize];
        mem.write_force(THUNK_BASE, &fill).unwrap();
        let mut p = Process {
            cpu: Cpu::new(),
            mem,
            vfs,
            modules: Vec::new(),
            thunks: Vec::new(),
            thunk_index: HashMap::new(),
            unimplemented_log: Vec::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdin: VecDeque::new(),
            handles: HashMap::new(),
            next_handle: 0x100,
            heap: crate::hle::heap::Heap::new(),
            vmem: BTreeMap::new(),
            vmem_next: 0x2_0000_0000,
            cmdline: String::new(),
            image_path: String::new(),
            current_directory: "C:\\".into(),
            env: vec![
                ("SystemRoot".into(), "C:\\Windows".into()),
                ("TEMP".into(), "C:\\Temp".into()),
                ("PATH".into(), "C:\\Windows\\System32".into()),
                ("COMPUTERNAME".into(), "ANYWASM".into()),
                ("OS".into(), "Windows_NT".into()),
            ],
            tls_next: 0,
            fls_next: 0,
            static_tls_next: 0,
            static_tls_templates: Vec::new(),
            fls: HashMap::new(),
            tls_slots: HashMap::new(),
            current_thread_id: 0x1004,
            next_thread_id: 0x1008,
            current_thread_is_guest: false,
            current_thread_state: ThreadState::Running,
            current_wait_order: 0,
            current_thread_saved: false,
            guest_threads: HashMap::new(),
            ready_threads: VecDeque::new(),
            schedule_requested: false,
            wait_sequence: 0,
            quantum_used: 0,
            retired_instructions: 0,
            srw_locks: HashMap::new(),
            critical_sections: HashMap::new(),
            atexit: Vec::new(),
            tick_ms: 0,
            qpc: 1_000_000,
            exit_code: None,
            rng: 0x2545_F491_4F6C_DD1D,
            trace_imports: false,
            nested: 0,
            skip_ret: false,
            yield_request: None,
            dcache: DecodeCache::new(),
            block_cache: BlockCache::new(),
            extra: hle::Extra::default(),
        };
        p.handles.insert(0xfffffffffffffff5, Handle::Std(1)); // pseudo values never used directly
        p
    }

    // ── guest memory helpers (faults become crashes) ─────────────────────────

    pub fn rd8(&self, a: u64) -> Result<u8, Stop> {
        self.mem
            .read_u8(a)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}: {e:?}", a)))
    }
    pub fn rd16(&self, a: u64) -> Result<u16, Stop> {
        self.mem
            .read_u16(a)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}: {e:?}", a)))
    }
    pub fn rd32(&self, a: u64) -> Result<u32, Stop> {
        self.mem
            .read_u32(a)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}: {e:?}", a)))
    }
    pub fn rd64(&self, a: u64) -> Result<u64, Stop> {
        self.mem
            .read_u64(a)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}: {e:?}", a)))
    }
    pub fn wr8(&mut self, a: u64, v: u8) -> Result<(), Stop> {
        self.mem
            .write_u8(a, v)
            .map_err(|e| Stop::Crash(format!("write fault {:#x}: {e:?}", a)))
    }
    pub fn wr16(&mut self, a: u64, v: u16) -> Result<(), Stop> {
        self.mem
            .write_u16(a, v)
            .map_err(|e| Stop::Crash(format!("write fault {:#x}: {e:?}", a)))
    }
    pub fn wr32(&mut self, a: u64, v: u32) -> Result<(), Stop> {
        self.mem
            .write_u32(a, v)
            .map_err(|e| Stop::Crash(format!("write fault {:#x}: {e:?}", a)))
    }
    pub fn wr64(&mut self, a: u64, v: u64) -> Result<(), Stop> {
        self.mem
            .write_u64(a, v)
            .map_err(|e| Stop::Crash(format!("write fault {:#x}: {e:?}", a)))
    }
    pub fn read_bytes(&self, a: u64, n: usize) -> Result<Vec<u8>, Stop> {
        self.mem
            .read_vec(a, n)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}+{n}: {e:?}", a)))
    }
    pub fn write_bytes(&mut self, a: u64, b: &[u8]) -> Result<(), Stop> {
        self.mem
            .write_from(a, b)
            .map_err(|e| Stop::Crash(format!("write fault {:#x}+{}: {e:?}", a, b.len())))
    }
    pub fn read_cstr(&self, a: u64) -> Result<String, Stop> {
        let b = self
            .mem
            .read_cstr(a, 1 << 20)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}: {e:?}", a)))?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }
    pub fn read_wstr(&self, a: u64) -> Result<String, Stop> {
        self.mem
            .read_wstr(a, 1 << 20)
            .map_err(|e| Stop::Crash(format!("read fault {:#x}: {e:?}", a)))
    }
    pub fn write_cstr(&mut self, a: u64, s: &str) -> Result<(), Stop> {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        self.write_bytes(a, &b)
    }
    pub fn write_wstr(&mut self, a: u64, s: &str) -> Result<(), Stop> {
        let mut b: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        b.extend([0, 0]);
        self.write_bytes(a, &b)
    }

    // ── calling convention ───────────────────────────────────────────────────

    /// Integer/pointer argument `n` of the HLE call currently in progress (Win64 ABI).
    pub fn arg(&self, n: usize) -> u64 {
        match n {
            0 => self.cpu.gpr[RCX],
            1 => self.cpu.gpr[RDX],
            2 => self.cpu.gpr[R8],
            3 => self.cpu.gpr[R9],
            _ => self
                .mem
                .read_u64(self.cpu.gpr[RSP].wrapping_add(8 + 8 * n as u64))
                .unwrap_or(0),
        }
    }
    pub fn argf(&self, n: usize) -> f64 {
        f64::from_bits(self.cpu.xmm[n] as u64)
    }
    pub fn set_retf(&mut self, v: f64) {
        self.cpu.xmm[0] = v.to_bits() as u128;
    }
    pub fn set_retf32(&mut self, v: f32) {
        self.cpu.xmm[0] = v.to_bits() as u128;
    }

    pub fn set_last_error(&mut self, e: u32) {
        let _ = self.mem.write_u32(TEB_ADDR + 0x68, e);
    }

    pub fn now_ticks(&self) -> u64 {
        self.retired_instructions
            .saturating_add(self.extra.sleep_adjust)
    }

    pub fn create_guest_thread(
        &mut self,
        id: u32,
        start: u64,
        parameter: u64,
        suspended: bool,
    ) -> Result<(), Stop> {
        let stack_limit = self.alloc_virtual(0x10000, PROT_RW);
        let stack_base = stack_limit + 0x10000;
        let stack_pointer = stack_base - 8;
        self.wr64(stack_pointer, THREAD_RETURN_MAGIC)?;
        let mut cpu = Cpu::new();
        cpu.rip = start;
        cpu.gpr[RSP] = stack_pointer;
        cpu.gpr[RCX] = parameter;
        cpu.gs_base = TEB_ADDR;
        let teb_tls_vector = self.build_static_tls_vector()?;
        let state = if suspended {
            ThreadState::Suspended
        } else {
            ThreadState::Ready
        };
        self.guest_threads.insert(
            id,
            GuestThread {
                cpu,
                stack_base,
                stack_limit,
                teb_tls_vector,
                last_error: 0,
                tls: HashMap::new(),
                fls: HashMap::new(),
                state,
                is_guest: true,
                wait_order: 0,
            },
        );
        if !suspended {
            self.ready_threads.push_back(id);
        }
        Ok(())
    }

    pub fn resume_guest_thread(&mut self, id: u32) -> bool {
        let Some(thread) = self.guest_threads.get_mut(&id) else {
            return false;
        };
        if thread.state != ThreadState::Suspended {
            return false;
        }
        thread.state = ThreadState::Ready;
        self.ready_threads.push_back(id);
        self.schedule_requested = true;
        true
    }

    pub fn block_current_thread(&mut self, wait: ThreadWait) {
        self.wait_sequence = self.wait_sequence.wrapping_add(1);
        self.current_wait_order = self.wait_sequence;
        self.current_thread_state = ThreadState::Blocked(wait);
        self.schedule_requested = true;
    }

    pub fn request_thread_yield(&mut self) {
        self.schedule_requested = true;
    }

    fn save_current_thread(&mut self) -> Result<(), Stop> {
        if self.current_thread_saved {
            return Ok(());
        }
        if self.current_thread_state == ThreadState::Exited(0) {
            return Ok(());
        }
        if matches!(self.current_thread_state, ThreadState::Exited(_)) {
            return Ok(());
        }
        let context = GuestThread {
            cpu: self.cpu.clone(),
            stack_base: self.rd64(TEB_ADDR + 0x08)?,
            stack_limit: self.rd64(TEB_ADDR + 0x10)?,
            teb_tls_vector: self.rd64(TEB_ADDR + 0x58)?,
            last_error: self.rd32(TEB_ADDR + 0x68)?,
            tls: std::mem::take(&mut self.tls_slots),
            fls: std::mem::take(&mut self.fls),
            state: self.current_thread_state.clone(),
            is_guest: self.current_thread_is_guest,
            wait_order: self.current_wait_order,
        };
        let id = self.current_thread_id;
        let enqueue = context.state == ThreadState::Ready;
        self.guest_threads.insert(id, context);
        if enqueue {
            self.ready_threads.push_back(id);
        }
        self.current_thread_saved = true;
        Ok(())
    }

    fn restore_thread(&mut self, id: u32) -> Result<(), Stop> {
        let Some(context) = self.guest_threads.remove(&id) else {
            return Err(Stop::Crash(format!(
                "ready thread {id} has no saved context"
            )));
        };
        self.cpu = context.cpu;
        self.current_thread_id = id;
        self.current_thread_is_guest = context.is_guest;
        self.current_wait_order = context.wait_order;
        self.current_thread_state = ThreadState::Running;
        self.current_thread_saved = false;
        self.tls_slots = context.tls;
        self.fls = context.fls;
        self.wr64(TEB_ADDR + 0x08, context.stack_base)?;
        self.wr64(TEB_ADDR + 0x10, context.stack_limit)?;
        self.wr64(TEB_ADDR + 0x58, context.teb_tls_vector)?;
        self.wr64(TEB_ADDR + 0x48, id as u64)?;
        self.wr32(TEB_ADDR + 0x68, context.last_error)?;
        self.cpu.gs_base = TEB_ADDR;
        Ok(())
    }

    /// Save the current continuation and select the next FIFO-ready guest thread.
    pub fn schedule_thread(&mut self) -> Result<bool, Stop> {
        self.schedule_requested = false;
        if self.current_thread_state == ThreadState::Running {
            self.current_thread_state = ThreadState::Ready;
        }
        self.save_current_thread()?;
        self.quantum_used = 0;
        while let Some(id) = self.ready_threads.pop_front() {
            if self
                .guest_threads
                .get(&id)
                .is_some_and(|thread| thread.state == ThreadState::Ready)
            {
                self.restore_thread(id)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Advance deterministic time to the next deadline when no guest thread is runnable.
    pub fn advance_to_next_deadline(&mut self) -> bool {
        let next = self
            .guest_threads
            .values()
            .filter_map(|thread| match &thread.state {
                ThreadState::Blocked(ThreadWait::Handle {
                    deadline: Some(deadline),
                    ..
                })
                | ThreadState::Blocked(ThreadWait::Multiple {
                    deadline: Some(deadline),
                    ..
                })
                | ThreadState::Blocked(ThreadWait::Sleep { deadline })
                | ThreadState::Blocked(ThreadWait::Condition {
                    deadline: Some(deadline),
                    outcome: None,
                    ..
                }) => Some(*deadline),
                _ => None,
            })
            .min();
        let Some(next) = next else {
            return false;
        };
        let now = self.now_ticks();
        self.extra.sleep_adjust = self
            .extra
            .sleep_adjust
            .saturating_add(next.saturating_sub(now));
        self.refresh_waiters();
        true
    }

    pub fn exit_current_guest_thread(&mut self, code: u32) {
        let id = self.current_thread_id;
        self.current_thread_state = ThreadState::Exited(code);
        for handle in self.handles.values_mut() {
            if let Handle::Thread {
                id: handle_id,
                exit_code,
                ..
            } = handle
            {
                if *handle_id == id {
                    *exit_code = Some(code);
                }
            }
        }
        self.schedule_requested = true;
    }

    pub fn try_acquire_srw(&mut self, address: u64, id: u32, exclusive: bool) -> bool {
        let state = self.srw_locks.entry(address).or_default();
        if !state.waiters.is_empty() && state.waiters.front().copied() != Some(id) {
            return false;
        }
        let available = if exclusive {
            state.exclusive_owner.is_none() && state.shared_owners.is_empty()
        } else {
            state.exclusive_owner.is_none()
        };
        if !available {
            return false;
        }
        if state.waiters.front().copied() == Some(id) {
            state.waiters.pop_front();
        }
        if exclusive {
            state.exclusive_owner = Some(id);
        } else {
            *state.shared_owners.entry(id).or_default() += 1;
        }
        true
    }

    pub fn release_srw(&mut self, address: u64, id: u32, exclusive: bool) -> bool {
        let Some(state) = self.srw_locks.get_mut(&address) else {
            return false;
        };
        if exclusive {
            if state.exclusive_owner != Some(id) {
                return false;
            }
            state.exclusive_owner = None;
        } else {
            let Some(count) = state.shared_owners.get_mut(&id) else {
                return false;
            };
            *count -= 1;
            if *count == 0 {
                state.shared_owners.remove(&id);
            }
        }
        true
    }

    pub fn try_acquire_critical(&mut self, address: u64, id: u32) -> bool {
        let state = self.critical_sections.entry(address).or_default();
        if state.owner == Some(id) {
            state.recursion = state.recursion.saturating_add(1);
            return true;
        }
        if state.owner.is_some()
            || (!state.waiters.is_empty() && state.waiters.front().copied() != Some(id))
        {
            return false;
        }
        if state.waiters.front().copied() == Some(id) {
            state.waiters.pop_front();
        }
        state.owner = Some(id);
        state.recursion = 1;
        true
    }

    pub fn release_critical(&mut self, address: u64, id: u32) -> bool {
        let Some(state) = self.critical_sections.get_mut(&address) else {
            return false;
        };
        if state.owner != Some(id) {
            return false;
        }
        state.recursion -= 1;
        if state.recursion == 0 {
            state.owner = None;
        }
        true
    }

    fn handle_is_signaled(&self, handle: u64, id: u32) -> Option<bool> {
        match self.handles.get(&handle)? {
            Handle::Event { signaled, .. } => Some(*signaled),
            Handle::Mutex { owner, .. } => Some(owner.is_none() || *owner == Some(id)),
            Handle::Semaphore { count, .. } => Some(*count != 0),
            Handle::Thread { exit_code, .. } => Some(exit_code.is_some()),
            _ => None,
        }
    }

    fn acquire_wait_handle(&mut self, handle: u64, id: u32) {
        match self
            .handles
            .get_mut(&handle)
            .expect("validated wait handle")
        {
            Handle::Event { signaled, manual } => {
                if !*manual {
                    *signaled = false;
                }
            }
            Handle::Mutex { owner, recursion } => {
                if *owner == Some(id) {
                    *recursion = recursion.saturating_add(1);
                } else {
                    *owner = Some(id);
                    *recursion = 1;
                }
            }
            Handle::Semaphore { count, .. } => *count -= 1,
            Handle::Thread { .. } => {}
            _ => unreachable!("validated wait handle"),
        }
    }

    /// Atomically acquire supported waitable handles when ready. Returns the wait-any
    /// index (or zero for wait-all), `None` if the wait should block, or an error code.
    pub fn try_wait_handles(
        &mut self,
        handles: &[u64],
        wait_all: bool,
        id: u32,
    ) -> Result<Option<u64>, u32> {
        if handles.is_empty() {
            return Err(87);
        }
        if wait_all
            && handles
                .iter()
                .enumerate()
                .any(|(i, handle)| handles[..i].contains(handle))
        {
            return Err(87);
        }
        for &handle in handles {
            if self.handle_is_signaled(handle, id).is_none() {
                return Err(6);
            }
        }
        if wait_all {
            if handles
                .iter()
                .any(|&handle| !self.handle_is_signaled(handle, id).unwrap_or(false))
            {
                return Ok(None);
            }
            for &handle in handles {
                self.acquire_wait_handle(handle, id);
            }
            return Ok(Some(0));
        }
        for (index, &handle) in handles.iter().enumerate() {
            if self.handle_is_signaled(handle, id) == Some(true) {
                self.acquire_wait_handle(handle, id);
                return Ok(Some(index as u64));
            }
        }
        Ok(None)
    }

    pub fn wake_condition(&mut self, condition: u64, all: bool) {
        self.refresh_waiters();
        let mut waiters = self
            .guest_threads
            .iter()
            .filter_map(|(&id, thread)| match &thread.state {
                ThreadState::Blocked(ThreadWait::Condition {
                    condition: c,
                    outcome: None,
                    ..
                }) if *c == condition => Some((thread.wait_order, id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        waiters.sort_unstable();
        if !all {
            waiters.truncate(1);
        }
        for (_, id) in waiters {
            if let Some(thread) = self.guest_threads.get_mut(&id) {
                if let ThreadState::Blocked(ThreadWait::Condition { outcome, .. }) =
                    &mut thread.state
                {
                    *outcome = Some(true);
                }
            }
        }
        self.refresh_waiters();
    }

    /// Complete signaled waits and deadlines in deterministic block order.
    pub fn refresh_waiters(&mut self) {
        let now = self.now_ticks();
        let mut blocked = self
            .guest_threads
            .iter()
            .filter_map(|(&id, thread)| {
                matches!(thread.state, ThreadState::Blocked(_)).then_some((thread.wait_order, id))
            })
            .collect::<Vec<_>>();
        blocked.sort_unstable();
        for (_, id) in blocked {
            let Some(wait) = self
                .guest_threads
                .get(&id)
                .and_then(|thread| match &thread.state {
                    ThreadState::Blocked(wait) => Some(wait.clone()),
                    _ => None,
                })
            else {
                continue;
            };
            let completion = match wait {
                ThreadWait::Handle { handle, deadline } => {
                    match self.try_wait_handles(&[handle], false, id) {
                        Ok(Some(_)) => Some((0, None)),
                        Ok(None) if deadline.is_some_and(|deadline| now >= deadline) => {
                            Some((0x102, None))
                        }
                        Err(error) => Some((u32::MAX as u64, Some(error))),
                        _ => None,
                    }
                }
                ThreadWait::Multiple {
                    handles,
                    wait_all,
                    deadline,
                } => match self.try_wait_handles(&handles, wait_all, id) {
                    Ok(Some(index)) => Some((index, None)),
                    Ok(None) if deadline.is_some_and(|deadline| now >= deadline) => {
                        Some((0x102, None))
                    }
                    Err(error) => Some((u32::MAX as u64, Some(error))),
                    _ => None,
                },
                ThreadWait::Sleep { deadline } if now >= deadline => Some((0, None)),
                ThreadWait::CriticalAcquire { address } => {
                    if self.try_acquire_critical(address, id) {
                        Some((0, None))
                    } else {
                        None
                    }
                }
                ThreadWait::SrwAcquire { address, exclusive } => {
                    if self.try_acquire_srw(address, id, exclusive) {
                        Some((0, None))
                    } else {
                        None
                    }
                }
                ThreadWait::Condition {
                    condition,
                    lock,
                    critical,
                    exclusive,
                    deadline,
                    mut outcome,
                } => {
                    if outcome.is_none() && deadline.is_some_and(|deadline| now >= deadline) {
                        outcome = Some(false);
                    }
                    if let Some(woken) = outcome {
                        let acquired = if critical {
                            self.try_acquire_critical(lock, id)
                        } else {
                            self.try_acquire_srw(lock, id, exclusive)
                        };
                        if !acquired {
                            if critical {
                                let state = self.critical_sections.entry(lock).or_default();
                                if !state.waiters.contains(&id) {
                                    state.waiters.push_back(id);
                                }
                            } else {
                                let state = self.srw_locks.entry(lock).or_default();
                                if !state.waiters.contains(&id) {
                                    state.waiters.push_back(id);
                                }
                            }
                            if let Some(thread) = self.guest_threads.get_mut(&id) {
                                thread.state = ThreadState::Blocked(ThreadWait::Condition {
                                    condition,
                                    lock,
                                    critical,
                                    exclusive,
                                    deadline,
                                    outcome: Some(woken),
                                });
                            }
                            None
                        } else {
                            Some((woken as u64, (!woken).then_some(1460)))
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some((result, error)) = completion {
                if let Some(thread) = self.guest_threads.get_mut(&id) {
                    thread.cpu.gpr[RAX] = result;
                    if let Some(error) = error {
                        thread.last_error = error;
                    }
                    thread.state = ThreadState::Ready;
                    self.ready_threads.push_back(id);
                }
            }
        }
    }

    pub fn new_handle(&mut self, h: Handle) -> u64 {
        let id = self.next_handle;
        self.next_handle += 4;
        self.handles.insert(id, h);
        id
    }

    pub fn next_rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    // ── thunks ───────────────────────────────────────────────────────────────

    /// Address that, when called, runs the host implementation of `group!name`.
    /// Returns an always-trapping thunk when `create_missing` and nothing implements it.
    pub fn thunk_for(
        &mut self,
        group: &'static str,
        name: &str,
        create_missing: bool,
    ) -> Option<u64> {
        if let Some(&a) = self.thunk_index.get(&(group, name.to_string())) {
            return Some(a);
        }
        let func = hle::lookup(group, name);
        if func.is_none() && !create_missing {
            return None;
        }
        let idx = self.thunks.len() as u64;
        assert!(idx < (THUNK_BYTES - 0x100) / 16, "thunk space exhausted");
        let addr = THUNK_BASE + idx * 16;
        self.thunks.push(Thunk {
            group,
            name: name.to_string(),
            func,
        });
        self.thunk_index.insert((group, name.to_string()), addr);
        Some(addr)
    }

    // ── loading ──────────────────────────────────────────────────────────────

    pub fn module_by_name(&self, name: &str) -> Option<usize> {
        let c = canonical_dll(base_name(name));
        self.modules
            .iter()
            .position(|m| canonical_dll(&m.name) == c)
    }

    pub fn module_by_base(&self, base: u64) -> Option<usize> {
        self.modules.iter().position(|m| m.base == base)
    }

    /// Register a host-provided module (kernel32 etc.) so GetModuleHandle/LoadLibrary succeed.
    pub fn register_hle_module(&mut self, name: &str, group: &'static str) -> usize {
        if let Some(i) = self.module_by_name(name) {
            return i;
        }
        let base = HLE_MODULE_BASE + (self.modules.len() as u64) * 0x10_0000;
        self.modules.push(Module {
            name: name.to_ascii_lowercase(),
            path: format!("C:\\Windows\\System32\\{}", name.to_ascii_lowercase()),
            base,
            size: 0x10_0000,
            entry: 0,
            is_dll: true,
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            hle_group: Some(group),
        });
        self.modules.len() - 1
    }

    /// Load the main executable (and, transitively, bundled DLLs) and prepare to run.
    pub fn load_exe(&mut self, path: &str, cmdline: Option<&str>) -> Result<(), String> {
        let data = self
            .vfs
            .get(path)
            .ok_or_else(|| format!("{path}: not in VFS"))?
            .clone();
        self.image_path = if path.contains(':') {
            path.to_string()
        } else {
            format!("C:\\{path}")
        };
        self.cmdline = cmdline
            .map(str::to_string)
            .unwrap_or_else(|| format!("\"{}\"", self.image_path));
        self.setup_teb_peb();
        let idx = self.load_image(&data, &self.image_path.clone(), false)?;
        let m = self.modules[idx].clone();
        self.mem
            .write_u64(PEB_ADDR + 0x10, m.base)
            .map_err(|e| format!("{e:?}"))?;
        // Stack
        let pe = Pe::parse(&data).map_err(|e| e.to_string())?;
        let reserve = pe.stack_reserve.clamp(0x10_0000, 0x100_0000);
        let stack_base = STACK_TOP - reserve;
        self.mem.map(stack_base, reserve, PROT_RW);
        let rsp = STACK_TOP - 0x28;
        self.cpu.gpr[RSP] = rsp;
        self.mem.write_u64(TEB_ADDR + 0x08, STACK_TOP).unwrap();
        self.mem.write_u64(TEB_ADDR + 0x10, stack_base).unwrap();
        // Return address of the entry point: exit with whatever it returned.
        self.cpu.gpr[RSP] -= 8;
        self.mem.write_u64(self.cpu.gpr[RSP], EXIT_MAGIC).unwrap();
        self.cpu.rip = m.entry;
        self.cpu.gpr[RCX] = PEB_ADDR;
        self.cpu.gs_base = TEB_ADDR;
        self.cpu.fs_base = 0;
        self.run_dll_inits(idx)?;
        self.cpu.rip = m.entry;
        Ok(())
    }

    fn setup_teb_peb(&mut self) {
        for (a, n) in [
            (PEB_ADDR, 0x1000u64),
            (TEB_ADDR, 0x2000),
            (PARAMS_ADDR, 0x2000),
            (LDR_ADDR, 0x1_0000),
        ] {
            self.mem.map(a, n, PROT_RW);
        }
        let m = &mut self.mem;
        m.write_u8(PEB_ADDR + 0x02, 0).unwrap(); // BeingDebugged = 0
        m.write_u64(PEB_ADDR + 0x18, LDR_ADDR).unwrap();
        m.write_u64(PEB_ADDR + 0x20, PARAMS_ADDR).unwrap();
        m.write_u32(PEB_ADDR + 0xb8, 1).unwrap(); // NumberOfProcessors
        m.write_u32(PEB_ADDR + 0x118, 10).unwrap(); // Windows 10.0.19045
        m.write_u32(PEB_ADDR + 0x11c, 0).unwrap();
        m.write_u16(PEB_ADDR + 0x120, 19045).unwrap();
        m.write_u32(PEB_ADDR + 0x124, 2).unwrap(); // VER_PLATFORM_WIN32_NT
        m.write_u64(TEB_ADDR + 0x30, TEB_ADDR).unwrap();
        m.write_u64(TEB_ADDR + 0x40, 0x1000).unwrap(); // ClientId.UniqueProcess
        m.write_u64(TEB_ADDR + 0x48, 0x1004).unwrap(); // ClientId.UniqueThread
        m.write_u64(TEB_ADDR + 0x60, PEB_ADDR).unwrap();
        // PEB_LDR_DATA with three empty circular lists.
        m.write_u32(LDR_ADDR, 0x58).unwrap();
        m.write_u8(LDR_ADDR + 4, 1).unwrap();
        for off in [0x10u64, 0x20, 0x30] {
            m.write_u64(LDR_ADDR + off, LDR_ADDR + off).unwrap();
            m.write_u64(LDR_ADDR + off + 8, LDR_ADDR + off).unwrap();
        }
        // RTL_USER_PROCESS_PARAMETERS: strings are filled in lazily by `finish_params`.
        m.write_u32(PARAMS_ADDR, 0x1000).unwrap();
        m.write_u32(PARAMS_ADDR + 4, 0x400).unwrap();
        // Heap bookkeeping needs a process heap handle.
        let h = self.heap.process_heap;
        self.mem.write_u64(PEB_ADDR + 0x30, h).unwrap();
        self.finish_params();
    }

    fn finish_params(&mut self) {
        let img = self.image_path.clone();
        let cmd = self.cmdline.clone();
        let strs: [(u64, &str); 3] = [(0x60, &img), (0x70, &cmd), (0x38, "C:\\")];
        for (i, (off, s)) in strs.iter().enumerate() {
            let buf = PARAMS_ADDR + 0x400 + (i as u64) * 0x400;
            let _ = self.write_wstr(buf, s);
            let bytes = (s.encode_utf16().count() * 2) as u16;
            let _ = self.mem.write_u16(PARAMS_ADDR + off, bytes);
            let _ = self.mem.write_u16(PARAMS_ADDR + off + 2, bytes + 2);
            let _ = self.mem.write_u64(PARAMS_ADDR + off + 8, buf);
        }
        // Standard handles (pseudo handles recognised by our HLE).
        let _ = self.mem.write_u64(PARAMS_ADDR + 0x20, 0x10);
        let _ = self.mem.write_u64(PARAMS_ADDR + 0x28, 0x14);
        let _ = self.mem.write_u64(PARAMS_ADDR + 0x30, 0x18);
    }

    /// Map a PE64 image. Returns the module index.
    pub fn load_image(
        &mut self,
        data: &[u8],
        path: &str,
        as_dll_hint: bool,
    ) -> Result<usize, String> {
        let pe = Pe::parse(data).map_err(|e| format!("{path}: {e}"))?;
        if !pe.is_64 || pe.machine != anywasm_pe::MACHINE_AMD64 {
            return Err(format!(
                "{path}: not a PE32+ x86-64 image (machine {:#x})",
                pe.machine
            ));
        }
        let runtime_functions = pe
            .runtime_functions()
            .map_err(|error| format!("{path}: {error}"))?;
        let exception_table_rva = pe.dirs[anywasm_pe::DIR_EXCEPTION].rva;
        let size = align_up(pe.size_of_image as u64, 0x1000);
        let base = if self.range_free(pe.image_base, size) {
            pe.image_base
        } else if pe.has_relocs() {
            self.mem
                .find_free(0x1_8000_0000, size + 0x1_0000)
                .ok_or("no address space for image")?
        } else {
            return Err(format!(
                "{path}: preferred base {:#x} is taken and the image has no relocations",
                pe.image_base
            ));
        };
        if !self.mem.map(base, size, PROT_RW) {
            return Err(format!("{path}: cannot map {size:#x} bytes at {base:#x}"));
        }
        // headers + sections
        let hdr = data
            .get(..pe.size_of_headers as usize)
            .ok_or("truncated headers")?;
        self.mem
            .write_force(base, hdr)
            .map_err(|e| format!("{e:?}"))?;
        for s in &pe.sections {
            let raw = data.get(
                s.raw_offset as usize
                    ..(s.raw_offset as usize + s.raw_size.min(s.mapped_size()) as usize),
            );
            if let Some(raw) = raw {
                self.mem
                    .write_force(base + s.virtual_address as u64, raw)
                    .map_err(|e| format!("{e:?}"))?;
            }
        }
        // relocations
        let delta = base.wrapping_sub(pe.image_base);
        if delta != 0 {
            for r in pe.relocations().map_err(|e| e.to_string())? {
                let a = base + r.rva as u64;
                match r.kind {
                    anywasm_pe::REL_BASED_DIR64 => {
                        let v = self.mem.read_u64(a).map_err(|e| format!("{e:?}"))?;
                        self.mem
                            .write_force(a, &v.wrapping_add(delta).to_le_bytes())
                            .map_err(|e| format!("{e:?}"))?;
                    }
                    anywasm_pe::REL_BASED_HIGHLOW => {
                        let v = self.mem.read_u32(a).map_err(|e| format!("{e:?}"))?;
                        self.mem
                            .write_force(a, &v.wrapping_add(delta as u32).to_le_bytes())
                            .map_err(|e| format!("{e:?}"))?;
                    }
                    _ => {}
                }
            }
        }
        let name = base_name(path).to_ascii_lowercase();
        // Reserve the module slot before resolving imports so circular dependencies terminate.
        let mut exports = HashMap::new();
        let mut ordinals = HashMap::new();
        let fwd: Vec<(String, String)>;
        {
            let mut f = Vec::new();
            for e in pe.exports().map_err(|e| e.to_string())? {
                if let Some(fw) = &e.forwarder {
                    if let Some(n) = &e.name {
                        f.push((n.clone(), fw.clone()));
                    }
                    continue;
                }
                let a = base + e.rva as u64;
                if let Some(n) = &e.name {
                    exports.insert(n.clone(), a);
                }
                ordinals.insert(e.ordinal, a);
            }
            fwd = f;
        }
        let is_dll = pe.is_dll() || as_dll_hint;
        let idx = self.modules.len();
        self.modules.push(Module {
            name,
            path: path.to_string(),
            base,
            size,
            entry: if pe.entry_rva != 0 {
                base + pe.entry_rva as u64
            } else {
                0
            },
            is_dll,
            exports,
            ordinals,
            exception_table_rva,
            runtime_functions,
            hle_group: None,
        });
        self.add_ldr_entry(idx);
        // imports
        for imp in pe.all_imports().map_err(|e| e.to_string())? {
            let provider = self.resolve_dll(&imp.dll)?;
            for e in &imp.entries {
                let addr = self.bind_import(provider, &imp.dll, &e.name)?;
                self.mem
                    .write_force(base + e.iat_rva as u64, &addr.to_le_bytes())
                    .map_err(|er| format!("{er:?}"))?;
            }
        }
        // forwarded exports are resolved after imports
        for (n, fw) in fwd {
            if let Some((dll, func)) = fw.split_once('.') {
                if let Ok(p) = self.resolve_dll(dll) {
                    if let Ok(a) = self.bind_import(
                        p,
                        dll,
                        &ImportName::Name {
                            hint: 0,
                            name: func.to_string(),
                        },
                    ) {
                        self.modules[idx].exports.insert(n, a);
                    }
                }
            }
        }
        // TLS
        if let Some(tls) = pe.tls().map_err(|e| e.to_string())? {
            self.setup_tls(base, &pe, &tls)?;
        }
        // final protections
        self.mem.protect(base, 0x1000, PROT_R);
        for s in &pe.sections {
            let mut prot = 0;
            if s.readable() {
                prot |= PROT_R;
            }
            if s.writable() {
                prot |= anywasm_cpu::mem::PROT_W;
            }
            if s.executable() {
                prot |= PROT_X;
            }
            if prot == 0 {
                prot = PROT_R;
            }
            let len = align_up(s.mapped_size() as u64, 0x1000);
            if len > 0 {
                self.mem.protect(base + s.virtual_address as u64, len, prot);
            }
        }
        Ok(idx)
    }

    fn range_free(&self, base: u64, size: u64) -> bool {
        let mut a = base & !0xfff;
        let end = base + size;
        while a < end {
            if self.mem.is_mapped(a) {
                return false;
            }
            a += 0x1000;
        }
        true
    }

    fn add_ldr_entry(&mut self, idx: usize) {
        let m = self.modules[idx].clone();
        let slot = LDR_ADDR + 0x100 + (idx as u64) * 0x200;
        if slot + 0x200 > LDR_ADDR + 0x1_0000 {
            return;
        }
        let _ = self.mem.write_u64(slot + 0x30, m.base);
        let _ = self.mem.write_u64(slot + 0x38, m.entry);
        let _ = self.mem.write_u32(slot + 0x40, m.size as u32);
        let full = slot + 0x80;
        let base_s = slot + 0x140;
        let _ = self.write_wstr(full, &m.path);
        let _ = self.write_wstr(base_s, &m.name);
        let fl = (m.path.encode_utf16().count() * 2) as u16;
        let bl = (m.name.encode_utf16().count() * 2) as u16;
        let _ = self.mem.write_u16(slot + 0x48, fl);
        let _ = self.mem.write_u16(slot + 0x4a, fl + 2);
        let _ = self.mem.write_u64(slot + 0x50, full);
        let _ = self.mem.write_u16(slot + 0x58, bl);
        let _ = self.mem.write_u16(slot + 0x5a, bl + 2);
        let _ = self.mem.write_u64(slot + 0x60, base_s);
        // Insert at the tail of the load-order and memory-order lists.
        for (head_off, link_off) in [(0x10u64, 0x00u64), (0x20, 0x10)] {
            let head = LDR_ADDR + head_off;
            let node = slot + link_off;
            let tail = self.mem.read_u64(head + 8).unwrap_or(head);
            let _ = self.mem.write_u64(node, head);
            let _ = self.mem.write_u64(node + 8, tail);
            let _ = self.mem.write_u64(tail, node);
            let _ = self.mem.write_u64(head + 8, node);
        }
    }

    /// Decide which module satisfies imports from `dll`: a bundled DLL or a host library.
    fn resolve_dll(&mut self, dll: &str) -> Result<usize, String> {
        if let Some(i) = self.module_by_name(dll) {
            return Ok(i);
        }
        // Bundled DLL?
        let relative = crate::vfs::absolute(dll, &self.current_directory);
        let file = if self.vfs.exists(&relative) {
            Some(crate::vfs::norm(&relative))
        } else if self.vfs.exists(dll) {
            Some(crate::vfs::norm(dll))
        } else {
            self.vfs.find_by_name(dll).cloned()
        };
        if let Some(f) = file {
            let data = self.vfs.get(&f).unwrap().clone();
            return self.load_image(&data, &f, true);
        }
        let group = hle::group_of(&canonical_dll(dll)).unwrap_or("unknown");
        Ok(self.register_hle_module(dll, group))
    }

    /// Address for one import: an export of a loaded guest DLL, or a host thunk.
    fn bind_import(
        &mut self,
        provider: usize,
        dll: &str,
        name: &ImportName,
    ) -> Result<u64, String> {
        let m = &self.modules[provider];
        match (&m.hle_group, name) {
            (None, ImportName::Name { name, .. }) => m
                .exports
                .get(name)
                .copied()
                .ok_or_else(|| format!("{}: export {name} not found", m.name)),
            (None, ImportName::Ordinal(o)) => m
                .ordinals
                .get(o)
                .copied()
                .ok_or_else(|| format!("{}: ordinal {o} not found", m.name)),
            (Some(g), ImportName::Name { name, .. }) => {
                let g: &'static str = g;
                let name = name.clone();
                if let Some(size) = hle::data_symbol(g, &name) {
                    return Ok(hle::crt::data_import(self, &name, size));
                }
                Ok(self.thunk_for(g, &name, true).unwrap())
            }
            (Some(g), ImportName::Ordinal(o)) => {
                let g: &'static str = g;
                let _ = dll;
                Ok(self.thunk_for(g, &format!("#{o}"), true).unwrap())
            }
        }
    }

    fn setup_tls(&mut self, base: u64, pe: &Pe, tls: &anywasm_pe::Tls) -> Result<(), String> {
        if self.current_thread_is_guest
            || self
                .guest_threads
                .values()
                .any(|thread| !matches!(thread.state, ThreadState::Exited(_)))
        {
            return Err(
                "loading a static-TLS module after guest-thread creation is unsupported".into(),
            );
        }
        let tmpl_len = tls.end_raw_data.saturating_sub(tls.start_raw_data);
        let total = tmpl_len + tls.zero_fill as u64;
        let data = if tmpl_len > 0 {
            let src = tls
                .start_raw_data
                .wrapping_sub(pe.image_base)
                .wrapping_add(base);
            self.mem
                .read_vec(src, tmpl_len as usize)
                .map_err(|e| format!("{e:?}"))?
        } else {
            Vec::new()
        };
        // ThreadLocalStoragePointer -> array of per-module blocks, index stored at AddressOfIndex.
        let mut arr = self.mem.read_u64(TEB_ADDR + 0x58).unwrap_or(0);
        if arr == 0 {
            arr = self.alloc_virtual(0x1000, PROT_RW);
            self.mem.write_u64(TEB_ADDR + 0x58, arr).unwrap();
        }
        let idx = self.static_tls_next;
        if idx >= 0x1000 / 8 {
            return Err("static TLS module index exceeds the supported 512-entry vector".into());
        }
        self.static_tls_next += 1;
        let block = self.alloc_virtual(align_up(total.max(16), 0x1000), PROT_RW);
        if !data.is_empty() {
            self.mem
                .write_force(block, &data)
                .map_err(|e| format!("{e:?}"))?;
        }
        self.mem
            .write_force(arr + idx as u64 * 8, &block.to_le_bytes())
            .map_err(|e| format!("{e:?}"))?;
        let idx_addr = tls
            .index_addr
            .wrapping_sub(pe.image_base)
            .wrapping_add(base);
        self.mem
            .write_force(idx_addr, &idx.to_le_bytes())
            .map_err(|e| format!("{e:?}"))?;
        self.static_tls_templates.push(StaticTlsTemplate {
            index: idx,
            data,
            total_size: total,
        });
        Ok(())
    }

    fn build_static_tls_vector(&mut self) -> Result<u64, Stop> {
        if self.static_tls_templates.is_empty() {
            return Ok(0);
        }
        let vector = self.alloc_virtual(0x1000, PROT_RW);
        for template in self.static_tls_templates.clone() {
            let block = self.alloc_virtual(align_up(template.total_size.max(16), 0x1000), PROT_RW);
            if !template.data.is_empty() {
                self.mem
                    .write_force(block, &template.data)
                    .map_err(|e| Stop::Crash(format!("static TLS template copy failed: {e:?}")))?;
            }
            self.wr64(vector + template.index as u64 * 8, block)?;
        }
        Ok(vector)
    }

    /// Reserve and map `size` bytes of anonymous virtual memory (64 KiB granularity).
    pub fn alloc_virtual(&mut self, size: u64, prot: u8) -> u64 {
        let size = align_up(size.max(1), 0x1000);
        let addr = self
            .mem
            .find_free(self.vmem_next, size)
            .expect("address space exhausted");
        self.mem.map(addr, size, prot);
        self.vmem.insert(addr, (size, prot));
        self.vmem_next = align_up(addr + size, 0x1_0000);
        addr
    }

    /// Run DllMain(DLL_PROCESS_ATTACH) for bundled DLLs (dependencies first), then TLS callbacks.
    fn run_dll_inits(&mut self, exe_idx: usize) -> Result<(), String> {
        let order: Vec<usize> = (0..self.modules.len())
            .filter(|&i| {
                i != exe_idx
                    && self.modules[i].hle_group.is_none()
                    && self.modules[i].entry != 0
                    && self.modules[i].is_dll
            })
            .collect();
        for i in order {
            let (base, entry) = (self.modules[i].base, self.modules[i].entry);
            let r = self.call_guest(entry, &[base, 1, 0]);
            match r {
                Ok(0) => return Err(format!("{}: DllMain returned FALSE", self.modules[i].name)),
                Ok(_) => {}
                Err(e) => return Err(format!("{}: DllMain failed: {e:?}", self.modules[i].name)),
            }
        }
        Ok(())
    }

    // ── execution ────────────────────────────────────────────────────────────

    /// Call a guest function with integer args and run until it returns. Re-entrant.
    pub fn call_guest(&mut self, addr: u64, args: &[u64]) -> Result<u64, Stop> {
        let saved = self.cpu.clone();
        // Build a Win64 frame below the current stack pointer: [shadow 0x20][stack args][ret addr].
        let nstack = args.len().saturating_sub(4) as u64;
        let mut sp = self.cpu.gpr[RSP].wrapping_sub(0x200) & !0xf;
        sp = (sp - 0x20 - nstack * 8) & !0xf;
        for (i, a) in args.iter().enumerate().skip(4) {
            self.wr64(sp + 0x20 + (i as u64 - 4) * 8, *a)?;
        }
        sp -= 8; // entry condition: rsp % 16 == 8
        self.wr64(sp, RET_MAGIC)?;
        for (i, r) in [RCX, RDX, R8, R9].iter().enumerate() {
            self.cpu.gpr[*r] = args.get(i).copied().unwrap_or(0);
        }
        self.cpu.gpr[RSP] = sp;
        self.cpu.rip = addr;
        self.nested += 1;
        let r = self.run_until_magic();
        self.nested -= 1;
        let ret = self.cpu.gpr[RAX];
        // Preserve nothing from the callee except the return value.
        let keep_exit = self.exit_code;
        self.cpu = saved;
        match r {
            Ok(()) => Ok(ret),
            Err(Stop::Exit(c)) => {
                self.exit_code = keep_exit.or(Some(c));
                Err(Stop::Exit(c))
            }
            Err(e) => Err(e),
        }
    }

    fn run_until_magic(&mut self) -> Result<(), Stop> {
        loop {
            if self.cpu.rip == RET_MAGIC {
                return Ok(());
            }
            match self.step_once()? {
                None => {}
                Some(stop) => return Err(stop),
            }
        }
    }

    /// Execute one instruction or one HLE call. `Ok(Some(stop))` only for terminal conditions.
    fn step_once(&mut self) -> Result<Option<Stop>, Stop> {
        let rip = self.cpu.rip;
        if rip >= THUNK_BASE && rip < THUNK_BASE + THUNK_BYTES {
            self.dispatch_thunk(rip)?;
            return Ok(self.yield_request.take());
        }
        match self.dcache.step(&mut self.cpu, &mut self.mem) {
            Ok(()) => Ok(None),
            Err(e) => Err(self.describe_exit(e)),
        }
    }

    fn execute_slice(&mut self, max_instructions: u64) -> Result<(u64, Option<Stop>), Stop> {
        let rip = self.cpu.rip;
        if rip >= THUNK_BASE && rip < THUNK_BASE + THUNK_BYTES {
            self.dispatch_thunk(rip)?;
            return Ok((1, self.yield_request.take()));
        }
        match self
            .block_cache
            .execute(&mut self.cpu, &mut self.mem, max_instructions)
        {
            Ok(executed) => Ok((executed, None)),
            Err(e) => Err(self.describe_exit(e)),
        }
    }

    fn describe_exit(&self, e: Exit) -> Stop {
        let c = &self.cpu;
        Stop::Crash(format!(
            "{:?} at rip={:#x} (module {}) rax={:#x} rcx={:#x} rdx={:#x} rsp={:#x}",
            e,
            c.rip,
            self.module_for(c.rip).unwrap_or("?"),
            c.gpr[RAX],
            c.gpr[RCX],
            c.gpr[RDX],
            c.gpr[RSP]
        ))
    }

    pub fn module_for(&self, addr: u64) -> Option<&str> {
        self.modules
            .iter()
            .find(|m| m.hle_group.is_none() && addr >= m.base && addr < m.base + m.size)
            .map(|m| m.name.as_str())
    }

    fn dispatch_thunk(&mut self, rip: u64) -> Result<(), Stop> {
        if rip == EXIT_MAGIC {
            let code = self.cpu.gpr[RAX] as u32;
            return self.finish_exit(code);
        }
        if rip == THREAD_RETURN_MAGIC {
            return Err(Stop::ThreadExit(0));
        }
        let idx = ((rip - THUNK_BASE) / 16) as usize;
        let Some(t) = self.thunks.get(idx) else {
            return Err(Stop::Crash(format!(
                "jump into unallocated thunk space at {rip:#x}"
            )));
        };
        let (group, name, func) = (t.group, t.name.clone(), t.func);
        if self.trace_imports {
            self.stderr
                .extend_from_slice(format!("[import] {group}!{name}\n").as_bytes());
        }
        let Some(f) = func else {
            self.unimplemented_log
                .push((group.to_string(), name.clone()));
            return Err(Stop::Unimplemented {
                dll: group.to_string(),
                name,
            });
        };
        self.skip_ret = false;
        let ret = f(self)?;
        self.refresh_waiters();
        if self.skip_ret {
            self.skip_ret = false;
            return Ok(());
        }
        // Return to the caller: rax = result, rip = [rsp], rsp += 8.
        let sp = self.cpu.gpr[RSP];
        let ra = self.rd64(sp)?;
        self.cpu.gpr[RAX] = ret;
        self.cpu.gpr[RSP] = sp + 8;
        self.cpu.rip = ra;
        Ok(())
    }

    /// Run atexit handlers, then record the exit code and stop.
    pub fn finish_exit(&mut self, code: u32) -> Result<(), Stop> {
        if self.exit_code.is_none() {
            while let Some(f) = self.atexit.pop() {
                let _ = self.call_guest(f, &[]);
            }
            self.exit_code = Some(code);
        }
        Err(Stop::Exit(self.exit_code.unwrap()))
    }

    /// Execute up to `max_steps` guest instructions (HLE calls count as one).
    pub fn run(&mut self, max_steps: u64) -> RunResult {
        if let Some(c) = self.exit_code {
            return Stop::Exit(c);
        }
        let mut remaining = max_steps;
        const THREAD_QUANTUM: u64 = 10_000;
        while remaining != 0 {
            self.refresh_waiters();
            if self.current_thread_state != ThreadState::Running
                || self.schedule_requested
                || self.quantum_used >= THREAD_QUANTUM
            {
                match self.schedule_thread() {
                    Ok(true) => {}
                    Ok(false) => {
                        if self.advance_to_next_deadline() && !self.ready_threads.is_empty() {
                            continue;
                        }
                        return Stop::Idle;
                    }
                    Err(stop) => return stop,
                }
            }
            let slice = remaining.min(THREAD_QUANTUM.saturating_sub(self.quantum_used).max(1));
            match self.execute_slice(slice) {
                Ok((_, Some(stop))) => return stop,
                Ok((executed, None)) => {
                    if executed == 0 {
                        return Stop::Crash(format!(
                            "block executor made no progress at rip={:#x}",
                            self.cpu.rip
                        ));
                    }
                    remaining -= executed;
                    self.retired_instructions = self.retired_instructions.saturating_add(executed);
                    self.quantum_used = self.quantum_used.saturating_add(executed);
                }
                Err(Stop::ThreadExit(code)) if self.current_thread_is_guest => {
                    remaining = remaining.saturating_sub(1);
                    self.retired_instructions = self.retired_instructions.saturating_add(1);
                    self.quantum_used = self.quantum_used.saturating_add(1);
                    self.exit_current_guest_thread(code);
                }
                Err(Stop::ThreadExit(code)) => {
                    return Stop::Crash(format!(
                        "ExitThread used by non-thread context with code {code}"
                    ))
                }
                Err(stop) => return stop,
            }
        }
        Stop::StepLimit
    }

    /// Run until the program stops for a reason other than the step budget.
    pub fn run_to_end(&mut self, max_steps: u64) -> RunResult {
        let mut left = max_steps;
        loop {
            let chunk = left.min(1_000_000);
            match self.run(chunk) {
                Stop::StepLimit => {
                    left -= chunk;
                    if left == 0 {
                        return Stop::StepLimit;
                    }
                }
                other => return other,
            }
        }
    }

    /// Take everything the guest has written to stdout so far.
    pub fn take_stdout(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.stdout)
    }
    pub fn take_stderr(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.stderr)
    }
}

pub fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    fn process_with_teb() -> Process {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        process.mem.write_u64(TEB_ADDR + 0x08, 0x7000).unwrap();
        process.mem.write_u64(TEB_ADDR + 0x10, 0x6000).unwrap();
        process.mem.write_u64(TEB_ADDR + 0x48, 0x1004).unwrap();
        process
    }

    #[test]
    fn scheduler_preserves_full_cpu_and_thread_local_state_across_yields() {
        let mut process = process_with_teb();
        process.cpu.rip = 0x1234;
        process.cpu.gpr[RBX] = 0xfeed_beef;
        process.cpu.xmm[7] = 0x1122_3344_5566_7788_aabb_ccdd_eeff_0011;
        process.tls_slots.insert(9, 0xaaaa);
        process
            .create_guest_thread(0x1008, 0x2000, 0x77, false)
            .unwrap();
        process.request_thread_yield();

        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.cpu.rip, 0x2000);
        assert_eq!(process.cpu.gpr[RCX], 0x77);
        process.cpu.gpr[RBX] = 0xcafe_babe;
        process.cpu.xmm[7] = 0x8877_6655_4433_2211_00ff_eedd_ccbb_aa99;
        process.tls_slots.insert(9, 0xbbbb);
        process.request_thread_yield();

        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1004);
        assert_eq!(process.cpu.rip, 0x1234);
        assert_eq!(process.cpu.gpr[RBX], 0xfeed_beef);
        assert_eq!(
            process.cpu.xmm[7],
            0x1122_3344_5566_7788_aabb_ccdd_eeff_0011
        );
        assert_eq!(process.tls_slots.get(&9), Some(&0xaaaa));

        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.cpu.gpr[RBX], 0xcafe_babe);
        assert_eq!(
            process.cpu.xmm[7],
            0x8877_6655_4433_2211_00ff_eedd_ccbb_aa99
        );
        assert_eq!(process.tls_slots.get(&9), Some(&0xbbbb));
    }

    #[test]
    fn static_tls_vector_and_blocks_are_private_and_restored_on_switch() {
        let mut process = process_with_teb();
        let main_vector = process.alloc_virtual(0x1000, PROT_RW);
        let main_block = process.alloc_virtual(0x1000, PROT_RW);
        process.wr64(TEB_ADDR + 0x58, main_vector).unwrap();
        process.wr64(main_vector, main_block).unwrap();
        process.wr32(main_block, 0x1111).unwrap();
        process.static_tls_templates.push(StaticTlsTemplate {
            index: 0,
            data: 7u32.to_le_bytes().to_vec(),
            total_size: 4,
        });

        process
            .create_guest_thread(0x1008, 0x2000, 0, false)
            .unwrap();
        let guest_vector = process.guest_threads.get(&0x1008).unwrap().teb_tls_vector;
        let guest_block = process.rd64(guest_vector).unwrap();
        assert_ne!(guest_vector, main_vector);
        assert_ne!(guest_block, main_block);
        assert_eq!(process.rd32(guest_block).unwrap(), 7);

        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.rd64(TEB_ADDR + 0x58).unwrap(), guest_vector);
        process.wr32(guest_block, 0x2222).unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1004);
        assert_eq!(process.rd64(TEB_ADDR + 0x58).unwrap(), main_vector);
        assert_eq!(process.rd32(main_block).unwrap(), 0x1111);

        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.rd64(TEB_ADDR + 0x58).unwrap(), guest_vector);
        let restored_guest_block = process
            .rd64(TEB_ADDR + 0x58)
            .and_then(|vector| process.rd64(vector))
            .unwrap();
        assert_eq!(process.rd32(restored_guest_block).unwrap(), 0x2222);
    }

    #[test]
    fn condition_waiter_reacquires_srw_lock_before_resume() {
        let mut process = process_with_teb();
        let lock = 0x4000;
        let condition = 0x5000;
        process
            .create_guest_thread(0x1008, 0x2000, 0, false)
            .unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert!(process.try_acquire_srw(lock, 0x1008, true));
        assert!(process.release_srw(lock, 0x1008, true));
        process.block_current_thread(ThreadWait::Condition {
            condition,
            lock,
            critical: false,
            exclusive: true,
            deadline: None,
            outcome: None,
        });
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1004);

        assert!(process.try_acquire_srw(lock, 0x1004, true));
        process.wake_condition(condition, false);
        assert!(matches!(
            process.guest_threads.get(&0x1008).unwrap().state,
            ThreadState::Blocked(ThreadWait::Condition {
                outcome: Some(true),
                ..
            })
        ));
        assert!(process.release_srw(lock, 0x1004, true));
        process.refresh_waiters();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.cpu.gpr[RAX], 1);
        assert_eq!(
            process.srw_locks.get(&lock).unwrap().exclusive_owner,
            Some(0x1008)
        );
    }

    #[test]
    fn wait_all_does_not_partially_consume_auto_reset_objects() {
        let mut process = process_with_teb();
        let first = process.new_handle(Handle::Event {
            signaled: true,
            manual: false,
        });
        let second = process.new_handle(Handle::Event {
            signaled: false,
            manual: false,
        });
        assert_eq!(
            process.try_wait_handles(&[first, second], true, 0x1004),
            Ok(None)
        );
        assert!(matches!(
            process.handles.get(&first),
            Some(Handle::Event { signaled: true, .. })
        ));
        if let Some(Handle::Event { signaled, .. }) = process.handles.get_mut(&second) {
            *signaled = true;
        }
        assert_eq!(
            process.try_wait_handles(&[first, second], true, 0x1004),
            Ok(Some(0))
        );
        assert!(matches!(
            process.handles.get(&first),
            Some(Handle::Event {
                signaled: false,
                ..
            })
        ));
        assert!(matches!(
            process.handles.get(&second),
            Some(Handle::Event {
                signaled: false,
                ..
            })
        ));
    }

    #[test]
    fn blocked_wait_all_resumes_only_after_every_object_is_ready() {
        let mut process = process_with_teb();
        let first = process.new_handle(Handle::Event {
            signaled: false,
            manual: false,
        });
        let second = process.new_handle(Handle::Event {
            signaled: false,
            manual: false,
        });
        process
            .create_guest_thread(0x1008, 0x2000, 0, false)
            .unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        process.block_current_thread(ThreadWait::Multiple {
            handles: vec![first, second],
            wait_all: true,
            deadline: None,
        });
        assert!(process.schedule_thread().unwrap());
        if let Some(Handle::Event { signaled, .. }) = process.handles.get_mut(&first) {
            *signaled = true;
        }
        process.refresh_waiters();
        assert!(matches!(
            process.guest_threads.get(&0x1008).unwrap().state,
            ThreadState::Blocked(_)
        ));
        if let Some(Handle::Event { signaled, .. }) = process.handles.get_mut(&second) {
            *signaled = true;
        }
        process.refresh_waiters();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.cpu.gpr[RAX], 0);
        assert!(matches!(
            process.handles.get(&first),
            Some(Handle::Event {
                signaled: false,
                ..
            })
        ));
        assert!(matches!(
            process.handles.get(&second),
            Some(Handle::Event {
                signaled: false,
                ..
            })
        ));
    }

    #[test]
    fn instruction_quantum_rotates_running_guest_threads_and_charges_global_budget() {
        let mut process = process_with_teb();
        let code = 0x10_000;
        assert!(process.mem.map(code, 0x1000, PROT_RWX));
        process.mem.write_force(code, &[0xeb, 0xfe]).unwrap(); // jmp to self
        process.cpu.rip = code;
        process.create_guest_thread(0x1008, code, 0, false).unwrap();
        process.request_thread_yield();

        assert_eq!(process.run(25_000), Stop::StepLimit);
        assert_eq!(process.retired_instructions, 25_000);
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.cpu.icount, 15_000);
        assert_eq!(
            process.guest_threads.get(&0x1004).unwrap().cpu.icount,
            10_000
        );
    }

    #[test]
    fn critical_section_condition_wait_reacquires_before_returning() {
        let mut process = process_with_teb();
        let lock = 0x6000;
        let condition = 0x7000;
        process
            .create_guest_thread(0x1008, 0x2000, 0, false)
            .unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert!(process.try_acquire_critical(lock, 0x1008));
        assert!(process.release_critical(lock, 0x1008));
        process.block_current_thread(ThreadWait::Condition {
            condition,
            lock,
            critical: true,
            exclusive: true,
            deadline: None,
            outcome: None,
        });
        assert!(process.schedule_thread().unwrap());
        assert!(process.try_acquire_critical(lock, 0x1004));
        process.wake_condition(condition, false);
        assert!(process.release_critical(lock, 0x1004));
        process.refresh_waiters();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(
            process.critical_sections.get(&lock).unwrap().owner,
            Some(0x1008)
        );
        assert_eq!(process.cpu.gpr[RAX], 1);
    }

    #[test]
    fn finite_condition_timeout_sets_last_error_after_lock_reacquisition() {
        let mut process = process_with_teb();
        let lock = 0x8000;
        let condition = 0x9000;
        process
            .create_guest_thread(0x1008, 0x2000, 0, false)
            .unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert!(process.try_acquire_srw(lock, 0x1008, true));
        assert!(process.release_srw(lock, 0x1008, true));
        process.block_current_thread(ThreadWait::Condition {
            condition,
            lock,
            critical: false,
            exclusive: true,
            deadline: Some(process.now_ticks() + 10),
            outcome: None,
        });
        assert!(process.schedule_thread().unwrap());
        assert!(process.advance_to_next_deadline());
        assert!(matches!(
            process.guest_threads.get(&0x1008).unwrap().state,
            ThreadState::Ready
        ));
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.cpu.gpr[RAX], 0);
        assert_eq!(process.rd32(TEB_ADDR + 0x68).unwrap(), 1460);
        assert_eq!(
            process.srw_locks.get(&lock).unwrap().exclusive_owner,
            Some(0x1008)
        );
    }
}
