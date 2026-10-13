//! Process state, PE64 loader and the run loop.

use crate::hle;
use crate::vfs::Vfs;
use anywasm_cpu::cpu::*;
use anywasm_cpu::mem::{PROT_R, PROT_RW, PROT_RWX, PROT_X};
use anywasm_cpu::{BlockCache, Cpu, CpuMode, DecodeCache, Exit, Memory};
use anywasm_pe::{canonical_dll, ImportName, Pe};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub const THUNK_BASE: u64 = 0x7100_0000;
pub const THUNK_BYTES: u64 = 0x10_0000;
/// Returning here ends a nested `call_guest`.
pub const RET_MAGIC: u64 = THUNK_BASE + THUNK_BYTES - 0x10;
/// Entry-point return lands here and exits the process with RAX.
pub const EXIT_MAGIC: u64 = THUNK_BASE + THUNK_BYTES - 0x20;
/// Return address installed on a newly created guest thread's stack.
pub const THREAD_RETURN_MAGIC: u64 = THUNK_BASE + THUNK_BYTES - 0x30;

pub const PEB_ADDR: u64 = 0x7ffc_0000;
pub const TEB_ADDR: u64 = 0x7ffc_1000;
pub const PARAMS_ADDR: u64 = 0x7ffc_4000;
pub const LDR_ADDR: u64 = 0x7ffd_0000;
pub const STACK_TOP: u64 = 0x7f00_0000;
pub const HLE_MODULE_BASE: u64 = 0x6000_0000;
pub const THREAD_QUANTUM: u64 = 10_000;
const CALLBACK_BUDGET_EXHAUSTED: &str =
    "synchronous guest callback exhausted the run budget; its continuation cannot be resumed";

fn trace_guest_instruction(cpu: &Cpu, mem: &Memory) -> String {
    let rip = if cpu.mode == CpuMode::X86_32 {
        cpu.rip as u32 as u64
    } else {
        cpu.rip
    };
    let bytes = (0..8)
        .map(|offset| mem.read_u8(rip.wrapping_add(offset)).ok())
        .take_while(Option::is_some)
        .flatten()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if cpu.mode == CpuMode::X86_32 {
        format!(
            "  rip={rip:#010x} rsp={:#010x} ebp={:#010x} eax={:#010x} ebx={:#010x} ecx={:#010x} edx={:#010x} esi={:#010x} edi={:#010x} code=[{bytes}]",
            cpu.gpr[RSP] as u32,
            cpu.gpr[RBP] as u32,
            cpu.gpr[RAX] as u32,
            cpu.gpr[RBX] as u32,
            cpu.gpr[RCX] as u32,
            cpu.gpr[RDX] as u32,
            cpu.gpr[RSI] as u32,
            cpu.gpr[RDI] as u32
        )
    } else {
        let names = [
            "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11",
            "r12", "r13", "r14", "r15",
        ];
        let regs = names
            .iter()
            .enumerate()
            .map(|(index, name)| format!("{name}={:#x}", cpu.gpr[index]))
            .collect::<Vec<_>>()
            .join(" ");
        format!("  rip={rip:#018x} {regs} code=[{bytes}]")
    }
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    fn yield_slice() -> u64;
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuestUnwind {
    pub target_frame: u64,
    pub target_ip: u64,
    pub call_rsp: u64,
    pub exception_record: u64,
    pub return_value: u64,
    pub context_record: u64,
    pub history_table: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GuestCatch32 {
    pub(crate) target_frame: u64,
    pub(crate) target_ip: u64,
    pub(crate) target_state: i32,
    pub(crate) catch_state: i32,
    pub(crate) try_index: usize,
    pub(crate) handler_index: usize,
}

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
    /// Internal marker consumed by the active guest exception dispatcher.
    GuestUnwind(GuestUnwind),
    /// Internal x86 C++ catch transfer consumed by the PE32 exception dispatcher.
    GuestCatch32(GuestCatch32),
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
    pub teb_base: u64,
    pub exception_list: u64,
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
    /// Relocated PE TLS process-attach callback addresses, in image order.
    pub tls_callbacks: Vec<u64>,
    pub exports: HashMap<String, u64>,
    pub ordinals: HashMap<u16, u64>,
    /// PE32+ exception directory and its validated, sorted `.pdata` entries.
    pub exception_table_rva: u32,
    pub runtime_functions: Vec<anywasm_pe::RuntimeFunction>,
    pub resource_rva: u32,
    pub resource_size: u32,
    /// Provided by the host (reimplemented library); `group` selects the function table.
    pub hle_group: Option<&'static str>,
}

/// Guest-registered x64 unwind table passed to `RtlAddFunctionTable`.
#[derive(Clone, Copy, Debug)]
pub struct DynamicFunctionTable {
    pub table: u64,
    pub entry_count: u32,
    pub base: u64,
}

pub struct Thunk {
    pub group: &'static str,
    pub name: String,
    pub func: Option<HleFn>,
}

struct SharedContextHandoff {
    previous: Option<(u32, u64, bool)>,
}

/// Win32 handle table entries.
pub enum Handle {
    Std(u32),
    File {
        path: String,
        pos: usize,
        write: bool,
        overlapped: bool,
    },
    Mapping {
        size: u64,
        file_path: Option<String>,
        protection: u8,
    },
    Find {
        entries: Vec<FindEntry>,
        next_index: usize,
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

#[derive(Clone, Debug)]
pub struct FindEntry {
    pub name: String,
    pub size: u64,
}

pub struct Process {
    pub cpu: Cpu,
    pub mem: Memory,
    pub vfs: Vfs,
    pub modules: Vec<Module>,
    pub dynamic_function_tables: Vec<DynamicFunctionTable>,
    pub(crate) vectored_exception_handlers: Vec<(u64, u64)>,
    pub(crate) vectored_continue_handlers: Vec<(u64, u64)>,
    pub(crate) next_vectored_exception_handle: u64,
    pub(crate) top_level_exception_filter: u64,
    module_dependencies: HashMap<usize, Vec<usize>>,
    initialized_dlls: HashSet<usize>,
    initializing_dlls: HashSet<usize>,
    pub thunks: Vec<Thunk>,
    pub thunk_index: HashMap<(&'static str, String), u64>,
    pub unimplemented_log: Vec<(String, String)>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdin: VecDeque<u8>,
    pending_input_events: VecDeque<(u32, i64, i64, i64)>,
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
    current_teb_base: u64,
    /// Active guest C++ exception unwind depth, kept per guest thread.
    pub guest_exception_depths: HashMap<u32, u32>,
    pub current_thread_is_guest: bool,
    pub current_thread_state: ThreadState,
    pub current_wait_order: u64,
    pub current_thread_saved: bool,
    pub guest_threads: HashMap<u32, GuestThread>,
    pub ready_threads: VecDeque<u32>,
    pub schedule_requested: bool,
    pub wait_sequence: u64,
    pub quantum_used: u64,
    shared_cpu_quantum_used: HashMap<u32, u64>,
    shared_context_handoff: Option<SharedContextHandoff>,
    pub retired_instructions: u64,
    active_run_budget: Option<u64>,
    pub srw_locks: HashMap<u64, SrwLockState>,
    pub critical_sections: HashMap<u64, CriticalSectionState>,
    pub atexit: Vec<u64>,
    pub tick_ms: u64,
    pub qpc: u64,
    pub exit_code: Option<u32>,
    pub rng: u64,
    pub trace_imports: bool,
    pub trace_modules_only: bool,
    pub trace_gta_eventvol: bool,
    trace_particle_txd: bool,
    trace_particle_texture_index: u32,
    trace_gta_copy_watch: Option<u32>,
    trace_recent_guest: VecDeque<String>,
    trace_recent_x86_ip: VecDeque<(u32, u32)>,
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

/// Number of stack argument bytes removed by an x86 Win32 callee. CRT and Vorbis C APIs use
/// caller cleanup. Keep this table explicit so an unknown signature cannot silently corrupt ESP.
fn stdcall_stack_bytes(group: &str, name: &str) -> u64 {
    if group == "crt" && name == "_CxxThrowException" {
        return 8;
    }
    if group == "com" {
        // COM methods use stdcall on x86, with `this` included in the callee-popped
        // argument list. Keep the shared IUnknown entries here; graphics and input
        // interfaces have additional signatures below.
        if name.ends_with("::QueryInterface") {
            return 12;
        }
        if name.ends_with("::AddRef") || name.ends_with("::Release") {
            return 4;
        }
        match name {
            // DirectDraw7::GetAvailableVidMem(this, caps, total, free)
            "IDirectDraw7::GetAvailableVidMem" => return 16,
            // DirectSound8::GetCaps(this, caps)
            "IDirectSound8::GetCaps" => return 8,
            _ => {}
        }
        if let Some(bytes) = hle::directinput8::stdcall_stack_bytes(name) {
            return bytes;
        }
        if let Some(bytes) = hle::d3d9::stdcall_stack_bytes(name) {
            return bytes;
        }
    }
    if group == "crt" || group == "vorbisfile" || name == "wsprintfA" {
        return 0;
    }
    if group == "kernel32" && name == "IsBadCodePtr" {
        return 4;
    }
    if group == "kernel32" {
        match name {
            "FindResourceA" | "FindResourceW" => return 12,
            "FindResourceExA" | "FindResourceExW" => return 16,
            "SizeofResource" => return 8,
            _ => {}
        }
    }
    if group == "user32" && matches!(name, "CharUpperA" | "CharLowerA") {
        return 4;
    }
    if group == "user32" && matches!(name, "CharUpperBuffA" | "CharLowerBuffA") {
        return 8;
    }
    if group == "user32" {
        let args = match name {
            "ReleaseCapture"
            | "GetMessageTime"
            | "GetInputState"
            | "GetForegroundWindow"
            | "GetDesktopWindow"
            | "GetActiveWindow" => Some(0),
            "GetCursorPos" | "GetSystemMetrics" | "GetAsyncKeyState" | "GetKeyboardType"
            | "GetKeyboardLayout" | "IsRectEmpty" | "GetKeyState" | "IsIconic" | "IsZoomed"
            | "ClipCursor" | "DestroyWindow" | "CharUpperA" | "CharLowerA" => Some(1),
            "GetWindowLongA"
            | "GetWindowThreadProcessId"
            | "GetWindowPlacement"
            | "GetWindowRect"
            | "GetClientRect"
            | "FindWindowA"
            | "ClientToScreen"
            | "SetCursorPos"
            | "SetWindowTextA"
            | "GetPropA"
            | "RemovePropA" => Some(2),
            "SetWindowLongA" | "SetPropA" | "IntersectRect" | "SubtractRect"
            | "MapVirtualKeyExA" | "GetKeyNameTextA" | "GetKeyNameTextW" => Some(3),
            "SetWindowsHookExA"
            | "PostMessageA"
            | "PostMessageW"
            | "PostThreadMessageA"
            | "SendMessageA"
            | "SendMessageW"
            | "SendNotifyMessageA"
            | "CallNextHookEx"
            | "DefWindowProcA"
            | "MapWindowPoints"
            | "SystemParametersInfoA"
            | "LoadStringA"
            | "KeybdEvent"
            | "keybd_event" => Some(4),
            "MsgWaitForMultipleObjects"
            | "PeekMessageA"
            | "CallWindowProcA"
            | "MouseEvent"
            | "mouse_event" => Some(5),
            "ToAsciiEx" => Some(6),
            _ => None,
        };
        if let Some(args) = args {
            return args * 4;
        }
    }
    let signatures: &[(&[&str], u64)] = match group {
        "kernel32" => &[
            (
                &[
                    "GetOEMCP",
                    "GetACP",
                    "GetTickCount",
                    "GetLastError",
                    "GetCurrentThread",
                    "GetCurrentThreadId",
                    "GetUserDefaultLCID",
                    "GetCurrentProcessId",
                    "GetSystemDefaultLCID",
                    "GetEnvironmentStrings",
                    "GetEnvironmentStringsW",
                    "GetCurrentProcess",
                    "GetProcessHeap",
                    "GetConsoleCP",
                    "GetConsoleOutputCP",
                    "GetVersion",
                    "TlsAlloc",
                    "GetCommandLineA",
                ],
                0,
            ),
            (
                &[
                    "IsValidCodePage",
                    "GetModuleHandleA",
                    "GetModuleHandleW",
                    "LoadLibraryA",
                    "LoadLibraryW",
                    "CloseHandle",
                    "LocalFree",
                    "GetThreadPriority",
                    "Sleep",
                    "SetLastError",
                    "DeleteFileA",
                    "GetFileAttributesA",
                    "FreeLibrary",
                    "OutputDebugStringA",
                    "GetLocalTime",
                    "SetErrorMode",
                    "SetCurrentDirectoryA",
                    "GetSystemTimeAsFileTime",
                    "QueryPerformanceCounter",
                    "QueryPerformanceFrequency",
                    "EncodePointer",
                    "DecodePointer",
                    "EncodeSystemPointer",
                    "DecodeSystemPointer",
                    "GetFileType",
                    "GetStdHandle",
                    "SetHandleCount",
                    "FlushFileBuffers",
                    "SetUnhandledExceptionFilter",
                    "TlsGetValue",
                    "TlsFree",
                    "FlsAlloc",
                    "FlsFree",
                    "FlsGetValue",
                    "GetStartupInfoA",
                    "GetStartupInfoW",
                    "GetVersionExA",
                    "GetVersionExW",
                    "FindClose",
                    "GetDriveTypeA",
                    "LockResource",
                    "UnmapViewOfFile",
                    "GetSystemInfo",
                    "IsProcessorFeaturePresent",
                    "ReleaseMutex",
                    "FreeEnvironmentStringsA",
                    "FreeEnvironmentStringsW",
                    "UnhandledExceptionFilter",
                    "HeapDestroy",
                    "ExitProcess",
                    "GetTimeZoneInformation",
                    "SetEndOfFile",
                    "GlobalMemoryStatus",
                    "DeleteCriticalSection",
                    "SuspendThread",
                    "ResumeThread",
                    "LeaveCriticalSection",
                    "EnterCriticalSection",
                    "InitializeCriticalSection",
                    "DisableThreadLibraryCalls",
                    "lstrlenA",
                    "lstrlenW",
                    "InterlockedIncrement",
                    "InterlockedDecrement",
                    "RemoveVectoredExceptionHandler",
                    "RemoveVectoredContinueHandler",
                ],
                1,
            ),
            (
                &[
                    "GetCPInfo",
                    "GetFileSize",
                    "GetProcAddress",
                    "WaitForSingleObject",
                    "SetStdHandle",
                    "SetThreadErrorMode",
                    "GetSystemDirectoryA",
                    "SetConsoleCtrlHandler",
                    "SetEnvironmentVariableA",
                    "lstrcatA",
                    "LoadResource",
                    "FatalAppExitA",
                    "CreateDirectoryA",
                    "GetLogicalDriveStringsA",
                    "EnumSystemLocalesA",
                    "GetCurrentDirectoryA",
                    "lstrcpyA",
                    "FindFirstFileA",
                    "FindNextFileA",
                    "SetCursorPos",
                    "InitializeCriticalSectionAndSpinCount",
                    "TlsSetValue",
                    "FlsSetValue",
                    "SetThreadPriority",
                    "LocalAlloc",
                    "GlobalAlloc",
                    "GetConsoleMode",
                    "TerminateProcess",
                    "InterlockedExchange",
                    "InterlockedExchange64",
                    "InterlockedExchangeAdd",
                    "InterlockedExchangeAdd64",
                    "InterlockedExchangePointer",
                    "SetFileAttributesA",
                    "SetFileAttributesW",
                    "FreeLibraryAndExitThread",
                    "IsBadWritePtr",
                    "IsBadReadPtr",
                ],
                2,
            ),
            (
                &["AddVectoredExceptionHandler", "AddVectoredContinueHandler"],
                2,
            ),
            (
                &[
                    "VirtualQuery",
                    "ReleaseSemaphore",
                    "CreateMutexA",
                    "GetModuleFileNameA",
                    "GetModuleFileNameW",
                    "VirtualFree",
                    "HeapCreate",
                    "HeapSize",
                    "HeapFree",
                    "SizeofResource",
                    "HeapAlloc",
                ],
                3,
            ),
            (
                &[
                    "VirtualProtect",
                    "WaitForSingleObjectEx",
                    "CreateSemaphoreA",
                    "CreateEventA",
                    "SetFilePointer",
                    "GetLocaleInfoA",
                    "GetLocaleInfoW",
                    "GetEnvironmentVariableA",
                    "GetEnvironmentVariableW",
                    "VirtualAlloc",
                    "HeapReAlloc",
                    "GetFullPathNameA",
                    "GetOverlappedResult",
                    "GetPrivateProfileIntA",
                    "GetPrivateProfileIntW",
                    "RtlUnwind",
                    "RaiseException",
                ],
                4,
            ),
            (
                &[
                    "GetStringTypeA",
                    "GetStringTypeW",
                    "ReadFile",
                    "WriteFile",
                    "WriteConsoleW",
                    "ReadConsoleW",
                    "MapViewOfFile",
                    "GetDiskFreeSpaceA",
                    "ReadFileEx",
                    "WriteFileEx",
                ],
                5,
            ),
            (
                &[
                    "CreateThread",
                    "MultiByteToWideChar",
                    "LCMapStringA",
                    "LCMapStringW",
                    "CreateFileMappingA",
                    "CompareStringA",
                    "CompareStringW",
                    "GetDateFormatA",
                    "GetTimeFormatA",
                ],
                6,
            ),
            (&["CreateFileA", "CreateFileW"], 7),
            (&["WideCharToMultiByte", "GetVolumeInformationA"], 8),
        ],
        "advapi32" => &[
            (&["RegCloseKey"], 1),
            (&["RegOpenKeyA"], 3),
            (&["RegOpenKeyExA"], 5),
            (&["RegQueryValueExA", "RegSetValueExA"], 6),
            (&["RegCreateKeyExA"], 9),
        ],
        "user32" => &[
            (
                &[
                    "ReleaseCapture",
                    "PostQuitMessage",
                    "SetCursor",
                    "ShowCursor",
                    "SetCapture",
                    "UpdateWindow",
                    "GetKeyState",
                    "SetForegroundWindow",
                    "DispatchMessageA",
                    "TranslateMessage",
                    "SetFocus",
                    "GetMenu",
                    "GetKeyboardLayout",
                    "SetCursorPos",
                    "UnhookWindowsHookEx",
                    "RegisterWindowMessageA",
                    "RegisterWindowMessageW",
                    "RegisterClassA",
                    "RegisterClassW",
                    "RegisterClassExA",
                    "RegisterClassExW",
                ],
                1,
            ),
            (
                &[
                    "GetWindowLongA",
                    "ShowWindow",
                    "LoadIconA",
                    "LoadCursorA",
                    "GetWindowPlacement",
                    "GetDlgItem",
                    "GetWindowRect",
                    "GetClientRect",
                    "FindWindowA",
                    "EndDialog",
                    "MapVirtualKeyA",
                    "ClientToScreen",
                    "SetWindowTextA",
                ],
                2,
            ),
            (&["AdjustWindowRect"], 3),
            (
                &[
                    "AdjustWindowRectEx",
                    "SystemParametersInfoA",
                    "SetTimer",
                    "DefWindowProcA",
                    "SendMessageA",
                    "MessageBoxA",
                    "SetWindowsHookExA",
                    "SetWindowsHookExW",
                    "CallNextHookEx",
                ],
                4,
            ),
            (&["PeekMessageA", "DialogBoxParamA"], 5),
            (&["SetWindowLongA"], 3),
            (&["SetWindowPos"], 7),
            (&["CreateWindowExA"], 12),
        ],
        "ole32" => &[
            (&["CoCreateInstance", "CoGetClassObject"], 5),
            (&["CoInitializeEx"], 2),
            (&["CoInitialize"], 1),
            (&["CoUninitialize"], 0),
        ],
        "d3d9" => &[(&["Direct3DCreate9"], 1)],
        "dinput8" => &[(&["DirectInput8Create"], 5)],
        "ddraw" => &[(&["DirectDrawCreateEx"], 4)],
        "dsound" => &[(&["#11"], 3)],
        "gdi32" => &[(&["DeleteObject"], 1)],
        "winmm" => &[
            (&["timeBeginPeriod", "timeEndPeriod"], 1),
            (&["timeGetDevCaps"], 2),
        ],
        "com" => &[
            (
                &[
                    "IDirectDraw7::QueryInterface",
                    "IDirectSound8::QueryInterface",
                    "IDirectSoundBuffer8::QueryInterface",
                    "IDirectSound3DListener::QueryInterface",
                ],
                3,
            ),
            (
                &[
                    "IDirectDraw7::AddRef",
                    "IDirectDraw7::Release",
                    "IDirectDraw7::Compact",
                    "IDirectDraw7::FlipToGDISurface",
                    "IDirectDraw7::RestoreDisplayMode",
                    "IDirectDraw7::RestoreAllSurfaces",
                    "IDirectDraw7::TestCooperativeLevel",
                ],
                1,
            ),
            (
                &[
                    "IDirectDraw7::GetDisplayMode",
                    "IDirectDraw7::GetGDISurface",
                    "IDirectDraw7::GetMonitorFrequency",
                    "IDirectDraw7::GetScanLine",
                    "IDirectDraw7::GetVerticalBlankStatus",
                    "IDirectDraw7::Initialize",
                ],
                2,
            ),
            (
                &[
                    "IDirectDraw7::GetFourCCCodes",
                    "IDirectDraw7::SetCooperativeLevel",
                    "IDirectDraw7::GetSurfaceFromDC",
                    "IDirectDraw7::GetDeviceIdentifier",
                    "IDirectDraw7::EvaluateMode",
                    "IDirectDraw7::DuplicateSurface",
                    "IDirectDraw7::GetCaps",
                    "IDirectDraw7::WaitForVerticalBlank",
                ],
                3,
            ),
            (
                &[
                    "IDirectDraw7::CreateClipper",
                    "IDirectDraw7::CreateSurface",
                    "IDirectDraw7::GetAvailableVidMem",
                    "IDirectDraw7::StartModeTest",
                ],
                4,
            ),
            (
                &[
                    "IDirectDraw7::EnumDisplayModes",
                    "IDirectDraw7::EnumSurfaces",
                    "IDirectDraw7::CreatePalette",
                ],
                5,
            ),
            (&["IDirectDraw7::SetDisplayMode"], 6),
            (
                &[
                    "IDirectSound8::AddRef",
                    "IDirectSound8::Release",
                    "IDirectSound8::Compact",
                    "IDirectSound3DListener::AddRef",
                    "IDirectSound3DListener::Release",
                    "IDirectSound3DListener::CommitDeferredSettings",
                ],
                1,
            ),
            (
                &[
                    "IDirectSoundBuffer8::AddRef",
                    "IDirectSoundBuffer8::Release",
                    "IDirectSoundBuffer8::Stop",
                    "IDirectSoundBuffer8::Restore",
                ],
                1,
            ),
            (
                &[
                    "IDirectSound8::GetCaps",
                    "IDirectSound8::GetSpeakerConfig",
                    "IDirectSound8::SetSpeakerConfig",
                    "IDirectSound8::Initialize",
                    "IDirectSound8::VerifyCertification",
                    "IDirectSound3DListener::GetAllParameters",
                    "IDirectSound3DListener::GetDistanceFactor",
                    "IDirectSound3DListener::GetDopplerFactor",
                    "IDirectSound3DListener::GetPosition",
                    "IDirectSound3DListener::GetRolloffFactor",
                    "IDirectSound3DListener::GetVelocity",
                ],
                2,
            ),
            (
                &[
                    "IDirectSound8::DuplicateSoundBuffer",
                    "IDirectSound8::SetCooperativeLevel",
                    "IDirectSound3DListener::GetOrientation",
                    "IDirectSound3DListener::SetAllParameters",
                    "IDirectSound3DListener::SetDistanceFactor",
                    "IDirectSound3DListener::SetDopplerFactor",
                    "IDirectSound3DListener::SetRolloffFactor",
                ],
                3,
            ),
            (&["IDirectSound8::CreateSoundBuffer"], 4),
            (
                &[
                    "IDirectSoundBuffer8::GetCaps",
                    "IDirectSoundBuffer8::GetVolume",
                    "IDirectSoundBuffer8::GetPan",
                    "IDirectSoundBuffer8::GetFrequency",
                    "IDirectSoundBuffer8::GetStatus",
                    "IDirectSoundBuffer8::SetCurrentPosition",
                    "IDirectSoundBuffer8::SetFormat",
                    "IDirectSoundBuffer8::SetVolume",
                    "IDirectSoundBuffer8::SetPan",
                    "IDirectSoundBuffer8::SetFrequency",
                ],
                2,
            ),
            (
                &[
                    "IDirectSoundBuffer8::GetCurrentPosition",
                    "IDirectSoundBuffer8::Initialize",
                ],
                3,
            ),
            (
                &[
                    "IDirectSoundBuffer8::GetFormat",
                    "IDirectSoundBuffer8::Play",
                    "IDirectSoundBuffer8::SetFX",
                    "IDirectSoundBuffer8::AcquireResources",
                ],
                4,
            ),
            (&["IDirectSoundBuffer8::Unlock"], 5),
            (&["IDirectSoundBuffer8::Lock"], 8),
            (&["IDirectSoundBuffer8::GetObjectInPath"], 5),
            (
                &[
                    "IDirectSound3DListener::SetPosition",
                    "IDirectSound3DListener::SetVelocity",
                ],
                5,
            ),
            (&["IDirectSound3DListener::SetOrientation"], 8),
        ],
        _ => &[],
    };
    let args = signatures
        .iter()
        .find_map(|(names, count)| names.contains(&name).then_some(*count))
        .unwrap_or(0);
    args * 4
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
            dynamic_function_tables: Vec::new(),
            vectored_exception_handlers: Vec::new(),
            vectored_continue_handlers: Vec::new(),
            next_vectored_exception_handle: 0x7000_0000,
            top_level_exception_filter: 0,
            module_dependencies: HashMap::new(),
            initialized_dlls: HashSet::new(),
            initializing_dlls: HashSet::new(),
            thunks: Vec::new(),
            thunk_index: HashMap::new(),
            unimplemented_log: Vec::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdin: VecDeque::new(),
            pending_input_events: VecDeque::new(),
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
            current_teb_base: TEB_ADDR,
            guest_exception_depths: HashMap::new(),
            current_thread_is_guest: false,
            current_thread_state: ThreadState::Running,
            current_wait_order: 0,
            current_thread_saved: false,
            guest_threads: HashMap::new(),
            ready_threads: VecDeque::new(),
            schedule_requested: false,
            wait_sequence: 0,
            quantum_used: 0,
            shared_cpu_quantum_used: HashMap::new(),
            shared_context_handoff: None,
            retired_instructions: 0,
            active_run_budget: None,
            srw_locks: HashMap::new(),
            critical_sections: HashMap::new(),
            atexit: Vec::new(),
            tick_ms: 0,
            qpc: 1_000_000,
            exit_code: None,
            rng: 0x2545_F491_4F6C_DD1D,
            trace_imports: false,
            trace_modules_only: false,
            trace_gta_eventvol: false,
            trace_particle_txd: false,
            trace_particle_texture_index: 0,
            trace_gta_copy_watch: None,
            trace_recent_guest: VecDeque::with_capacity(128),
            trace_recent_x86_ip: VecDeque::with_capacity(128),
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
    pub fn rdptr(&self, a: u64) -> Result<u64, Stop> {
        if self.cpu.mode == CpuMode::X86_32 {
            Ok(self.rd32(a)? as u64)
        } else {
            self.rd64(a)
        }
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
    pub fn wrptr(&mut self, a: u64, v: u64) -> Result<(), Stop> {
        if self.cpu.mode == CpuMode::X86_32 {
            self.wr32(a, v as u32)
        } else {
            self.wr64(a, v)
        }
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

    /// Integer/pointer argument `n` of the HLE call currently in progress.
    pub fn arg(&self, n: usize) -> u64 {
        if self.cpu.mode == CpuMode::X86_32 {
            return self
                .mem
                .read_u32(self.cpu.gpr[RSP].wrapping_add(4 + 4 * n as u64))
                .unwrap_or(0) as u64;
        }
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
        let offset = if self.cpu.mode == CpuMode::X86_32 {
            0x34
        } else {
            0x68
        };
        let _ = self.mem.write_u32(TEB_ADDR + offset, e);
    }

    pub fn now_ticks(&self) -> u64 {
        self.retired_instructions
            .saturating_add(self.extra.sleep_adjust)
    }

    /// Convert host elapsed 100 ns units to the guest clock's 10 ns ticks after a frame yield.
    pub fn advance_time_100ns(&mut self, ticks: u64) {
        self.extra.sleep_adjust = self
            .extra
            .sleep_adjust
            .saturating_add(ticks.saturating_mul(10));
    }

    fn allocate_guest_thread_teb(
        &mut self,
        thread_id: u32,
        stack_base: u64,
        stack_limit: u64,
        tls_vector: u64,
    ) -> Result<u64, Stop> {
        if self.cpu.mode == CpuMode::X86_32 {
            return Ok(TEB_ADDR);
        }
        let page_size = 0x1000u64;
        let mut bytes = self
            .mem
            .read_vec(TEB_ADDR, page_size as usize)
            .map_err(|error| Stop::Crash(format!("read coordinator TEB: {error:?}")))?;
        let teb_base = self
            .try_alloc_virtual(page_size, PROT_RW)
            .ok_or_else(|| Stop::Crash("guest allocation failed while allocating thread TEB".into()))?;
        let process_id = self.rd64(TEB_ADDR + 0x40)?;
        bytes[0x00..0x08].copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[0x08..0x10].copy_from_slice(&stack_base.to_le_bytes());
        bytes[0x10..0x18].copy_from_slice(&stack_limit.to_le_bytes());
        bytes[0x30..0x38].copy_from_slice(&teb_base.to_le_bytes());
        bytes[0x40..0x48].copy_from_slice(&process_id.to_le_bytes());
        bytes[0x48..0x50].copy_from_slice(&(thread_id as u64).to_le_bytes());
        bytes[0x58..0x60].copy_from_slice(&tls_vector.to_le_bytes());
        bytes[0x60..0x68].copy_from_slice(&PEB_ADDR.to_le_bytes());
        bytes[0x68..0x6c].copy_from_slice(&0u32.to_le_bytes());

        if let Err(error) = self.mem.write_force(teb_base, &bytes) {
            self.mem.unmap(teb_base, page_size);
            self.vmem.remove(&teb_base);
            return Err(Stop::Crash(format!(
                "initialize guest TEB page at {teb_base:#x}: {error:?}"
            )));
        }
        Ok(teb_base)
    }

    fn copy_teb_page(&mut self, source: u64, destination: u64) -> Result<(), Stop> {
        if source == destination {
            return Ok(());
        }
        let bytes = self
            .mem
            .read_vec(source, 0x1000)
            .map_err(|error| Stop::Crash(format!("read guest TEB at {source:#x}: {error:?}")))?;
        self.mem
            .write_force(destination, &bytes)
            .map_err(|error| Stop::Crash(format!("write guest TEB at {destination:#x}: {error:?}")))
    }

    fn sync_current_teb_to_backing(&mut self) -> Result<(), Stop> {
        self.copy_teb_page(TEB_ADDR, self.current_teb_base)?;
        if self.current_teb_base != TEB_ADDR && self.cpu.mode == CpuMode::X86_64 {
            self.wr64(self.current_teb_base + 0x30, self.current_teb_base)?;
        }
        Ok(())
    }

    fn load_teb_backing_into_alias(&mut self, teb_base: u64) -> Result<(), Stop> {
        self.copy_teb_page(teb_base, TEB_ADDR)?;
        if teb_base != TEB_ADDR && self.cpu.mode == CpuMode::X86_64 {
            self.wr64(TEB_ADDR + 0x30, TEB_ADDR)?;
        }
        Ok(())
    }

    pub fn create_guest_thread(
        &mut self,
        id: u32,
        start: u64,
        parameter: u64,
        suspended: bool,
    ) -> Result<(), Stop> {
        let stack_limit = self
            .try_alloc_virtual(0x10000, PROT_RW)
            .ok_or_else(|| Stop::Crash("guest allocation failed while allocating thread stack".into()))?;
        let stack_base = stack_limit + 0x10000;
        let stack_pointer = stack_base - 8;
        self.wrptr(stack_pointer, THREAD_RETURN_MAGIC)?;
        let mut cpu = Cpu::new();
        cpu.mode = self.cpu.mode;
        cpu.rip = start;
        cpu.gpr[RSP] = stack_pointer;
        if cpu.mode == CpuMode::X86_32 {
            self.wr32(stack_pointer + 4, parameter as u32)?;
            cpu.fs_base = TEB_ADDR;
        } else {
            cpu.gpr[RCX] = parameter;
            cpu.gs_base = TEB_ADDR;
        }
        let teb_tls_vector = self.build_static_tls_vector()?;
        let teb_base = self.allocate_guest_thread_teb(
            id,
            stack_base,
            stack_limit,
            teb_tls_vector,
        )?;
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
                teb_base,
                exception_list: u32::MAX as u64,
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

    /// Runnable contexts in coordinator scheduling order, with the active context first.
    /// Blocked, suspended, exited, and stale ready-queue entries are omitted.
    pub fn shared_cpu_runnable_context_ids(&self) -> Vec<u32> {
        let active = (!self.current_thread_saved
            && !self.schedule_requested
            && self.current_thread_state == ThreadState::Running
            && self.quantum_used < THREAD_QUANTUM)
            .then_some(self.current_thread_id);
        // PE32 threads still use the legacy fixed TEB alias. Keep their shared-worker execution
        // serialized until each PE32 thread has independent segment-base storage.
        if self.cpu.mode == CpuMode::X86_32 {
            return active.into_iter().collect();
        }
        let mut ids = Vec::new();
        if let Some(id) = active {
            ids.push(id);
        }

        for id in &self.ready_threads {
            if Some(*id) == active || ids.contains(id) {
                continue;
            }
            if self.shared_cpu_context_quantum_remaining(*id) != 0 {
                ids.push(*id);
            }
        }
        ids
    }

    /// Remaining instructions in this context's current scheduler quantum.
    pub fn shared_cpu_context_quantum_remaining(&self, thread_id: u32) -> u64 {
        if !self.current_thread_saved
            && !self.schedule_requested
            && self.current_thread_id == thread_id
            && self.current_thread_state == ThreadState::Running
        {
            return THREAD_QUANTUM.saturating_sub(self.quantum_used);
        }
        if self
            .guest_threads
            .get(&thread_id)
            .is_some_and(|thread| thread.state == ThreadState::Ready)
        {
            return THREAD_QUANTUM.saturating_sub(
                self.shared_cpu_quantum_used
                    .get(&thread_id)
                    .copied()
                    .unwrap_or(0),
            );
        }
        0
    }

    /// Retired guest instructions for an active or saved context.
    pub fn shared_cpu_context_instruction_count(&self, thread_id: u32) -> Option<u64> {
        if !self.current_thread_saved && self.current_thread_id == thread_id {
            Some(self.cpu.icount)
        } else {
            self.guest_threads
                .get(&thread_id)
                .map(|thread| thread.cpu.icount)
        }
    }

    /// TEB backing used when exporting a context to a shared CPU worker.
    pub fn shared_cpu_context_teb_base(&self, thread_id: u32) -> Option<u64> {
        if !self.current_thread_saved && self.current_thread_id == thread_id {
            Some(self.current_teb_base)
        } else {
            self.guest_threads
                .get(&thread_id)
                .map(|thread| thread.teb_base)
        }
    }

    /// Publish coordinator-written TEB fields into the guest-visible per-thread page.
    pub fn sync_shared_cpu_context_teb(&mut self, thread_id: u32) -> Result<bool, Stop> {
        if !self.current_thread_saved && self.current_thread_id == thread_id {
            self.sync_current_teb_to_backing()?;
            Ok(true)
        } else {
            Ok(self.guest_threads.contains_key(&thread_id))
        }
    }

    /// Load a worker-written active TEB page into the fixed coordinator HLE alias.
    pub fn sync_shared_cpu_context_alias(&mut self, thread_id: u32) -> Result<bool, Stop> {
        if !self.current_thread_saved && self.current_thread_id == thread_id {
            self.load_teb_backing_into_alias(self.current_teb_base)?;
            Ok(true)
        } else {
            Ok(self.guest_threads.contains_key(&thread_id))
        }
    }

    /// Account a completed worker slice against the owning thread and process exactly once.
    pub fn account_shared_cpu_context_retired(&mut self, thread_id: u32, retired: u64) {
        self.retired_instructions = self.retired_instructions.saturating_add(retired);
        if !self.current_thread_saved
            && self.current_thread_id == thread_id
            && self.current_thread_state == ThreadState::Running
        {
            self.quantum_used = self.quantum_used.saturating_add(retired);
        } else if self
            .guest_threads
            .get(&thread_id)
            .is_some_and(|thread| thread.state == ThreadState::Ready)
        {
            let used = self.shared_cpu_quantum_used.entry(thread_id).or_default();
            *used = used.saturating_add(retired).min(THREAD_QUANTUM);
        }
    }

    /// Temporarily make a completed saved context active for one coordinator-owned HLE/fault turn.
    /// Returns `false` when the requested context is already active.
    pub fn begin_shared_context_handoff(&mut self, thread_id: u32) -> Result<bool, Stop> {
        if self.shared_context_handoff.is_some() {
            return Err(Stop::Crash(
                "shared CPU coordinator handoff is already active".into(),
            ));
        }
        if !self.current_thread_saved
            && self.current_thread_id == thread_id
            && self.current_thread_state == ThreadState::Running
        {
            return Ok(false);
        }
        if !self
            .guest_threads
            .get(&thread_id)
            .is_some_and(|thread| thread.state == ThreadState::Ready)
        {
            return Err(Stop::Crash(format!(
                "shared CPU completion for non-runnable thread {thread_id}"
            )));
        }

        let previous = if self.current_thread_saved {
            None
        } else {
            let previous_id = self.current_thread_id;
            let previous_quantum = self.quantum_used;
            let resume_directly = self.current_thread_state == ThreadState::Running
                && !self.schedule_requested
                && previous_quantum < THREAD_QUANTUM;
            if self.current_thread_state == ThreadState::Running {
                self.current_thread_state = ThreadState::Ready;
            }
            let previous_was_exited = matches!(self.current_thread_state, ThreadState::Exited(_));
            self.save_current_thread()?;
            (!previous_was_exited).then_some((previous_id, previous_quantum, resume_directly))
        };

        let selected_quantum = self
            .shared_cpu_quantum_used
            .get(&thread_id)
            .copied()
            .unwrap_or(0);
        self.ready_threads.retain(|id| *id != thread_id);
        self.restore_thread(thread_id)?;
        self.quantum_used = selected_quantum;
        self.schedule_requested = false;
        self.shared_context_handoff = Some(SharedContextHandoff { previous });
        Ok(true)
    }

    /// Save the serviced context and restore the prior active context or resume scheduling.
    pub fn finish_shared_context_handoff(&mut self) -> Result<(), Stop> {
        let Some(handoff) = self.shared_context_handoff.take() else {
            return Err(Stop::Crash(
                "shared CPU coordinator handoff was not active".into(),
            ));
        };
        if self.exit_code.is_some() {
            return Ok(());
        }

        if !self.current_thread_saved {
            let serviced_id = self.current_thread_id;
            let serviced_quantum = self.quantum_used;
            if self.current_thread_state == ThreadState::Running {
                self.current_thread_state = ThreadState::Ready;
            }
            let serviced_ready = self.current_thread_state == ThreadState::Ready;
            self.save_current_thread()?;
            if serviced_ready {
                self.shared_cpu_quantum_used.insert(
                    serviced_id,
                    serviced_quantum.min(THREAD_QUANTUM),
                );
            }
        }

        if let Some((previous_id, previous_quantum, resume_directly)) = handoff.previous {
            // A serviced HLE may request a scheduler yield (for example SwitchToThread).
            // Honor that request instead of restoring the previous context unconditionally.
            if resume_directly
                && !self.schedule_requested
                && self
                    .guest_threads
                    .get(&previous_id)
                    .is_some_and(|thread| thread.state == ThreadState::Ready)
            {
                self.ready_threads.retain(|id| *id != previous_id);
                self.restore_thread(previous_id)?;
                self.quantum_used = previous_quantum;
                self.schedule_requested = false;
                return Ok(());
            }
        }

        self.schedule_requested = true;
        let _ = self.schedule_thread()?;
        Ok(())
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
        self.sync_current_teb_to_backing()?;
        let (stack_base_offset, stack_limit_offset, tls_vector_offset, last_error_offset) =
            if self.cpu.mode == CpuMode::X86_32 {
                (0x04, 0x08, 0x2c, 0x34)
            } else {
                (0x08, 0x10, 0x58, 0x68)
            };
        let context = GuestThread {
            cpu: self.cpu.clone(),
            stack_base: self.rdptr(TEB_ADDR + stack_base_offset)?,
            stack_limit: self.rdptr(TEB_ADDR + stack_limit_offset)?,
            teb_base: self.current_teb_base,
            exception_list: self.rdptr(TEB_ADDR)?,
            teb_tls_vector: self.rdptr(TEB_ADDR + tls_vector_offset)?,
            last_error: self.rd32(TEB_ADDR + last_error_offset)?,
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
        self.shared_cpu_quantum_used.remove(&id);
        self.current_teb_base = context.teb_base;
        self.load_teb_backing_into_alias(context.teb_base)?;
        self.cpu = context.cpu;
        self.current_thread_id = id;
        self.current_thread_is_guest = context.is_guest;
        self.current_wait_order = context.wait_order;
        self.current_thread_state = ThreadState::Running;
        self.current_thread_saved = false;
        self.tls_slots = context.tls;
        self.fls = context.fls;
        if self.cpu.mode == CpuMode::X86_32 {
            self.wr32(TEB_ADDR, context.exception_list as u32)?;
            self.wr32(TEB_ADDR + 0x04, context.stack_base as u32)?;
            self.wr32(TEB_ADDR + 0x08, context.stack_limit as u32)?;
            self.wr32(TEB_ADDR + 0x2c, context.teb_tls_vector as u32)?;
            self.wr32(TEB_ADDR + 0x24, id)?;
            self.wr32(TEB_ADDR + 0x34, context.last_error)?;
            self.cpu.fs_base = TEB_ADDR;
            self.cpu.gs_base = 0;
        } else {
            // The x64 TEB page is already thread-owned. Preserve guest-written fields from it
            // instead of overwriting them with the scheduler's older scalar snapshot.
            self.cpu.gs_base = TEB_ADDR;
            self.cpu.fs_base = 0;
        }
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
        self.shared_cpu_quantum_used.remove(&id);
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
            tls_callbacks: Vec::new(),
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: 0,
            resource_size: 0,
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
        let pe = Pe::parse(&data).map_err(|e| format!("{path}: {e}"))?;
        self.cpu.mode = match (pe.is_64, pe.machine) {
            (true, anywasm_pe::MACHINE_AMD64) => CpuMode::X86_64,
            (false, anywasm_pe::MACHINE_I386) => CpuMode::X86_32,
            _ => {
                return Err(format!(
                    "{path}: unsupported PE machine/magic combination (machine {:#x}, PE32+={})",
                    pe.machine, pe.is_64
                ));
            }
        };
        if self.cpu.mode == CpuMode::X86_32 {
            self.vmem_next = 0x2000_0000;
        }
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
        if self.cpu.mode == CpuMode::X86_32 {
            self.mem
                .write_u32(PEB_ADDR + 0x08, m.base as u32)
                .map_err(|e| format!("{e:?}"))?;
        } else {
            self.mem
                .write_u64(PEB_ADDR + 0x10, m.base)
                .map_err(|e| format!("{e:?}"))?;
        }
        // Stack
        let reserve = pe.stack_reserve.clamp(0x10_0000, 0x100_0000);
        let stack_base = STACK_TOP - reserve;
        self.mem.map(stack_base, reserve, PROT_RW);
        if self.cpu.mode == CpuMode::X86_32 {
            self.cpu.gpr[RSP] = STACK_TOP - 4;
            self.cpu.gpr[RSP] -= 4;
            self.mem
                .write_u32(self.cpu.gpr[RSP], EXIT_MAGIC as u32)
                .unwrap();
            self.mem
                .write_u32(TEB_ADDR + 0x04, STACK_TOP as u32)
                .unwrap();
            self.mem
                .write_u32(TEB_ADDR + 0x08, stack_base as u32)
                .unwrap();
            self.cpu.fs_base = TEB_ADDR;
            self.cpu.gs_base = 0;
        } else {
            self.cpu.gpr[RSP] = STACK_TOP - 0x28;
            self.mem.write_u64(TEB_ADDR + 0x08, STACK_TOP).unwrap();
            self.mem.write_u64(TEB_ADDR + 0x10, stack_base).unwrap();
            self.cpu.gpr[RSP] -= 8;
            self.mem.write_u64(self.cpu.gpr[RSP], EXIT_MAGIC).unwrap();
            self.cpu.gs_base = TEB_ADDR;
            self.cpu.fs_base = 0;
        }
        self.cpu.rip = m.entry;
        self.cpu.gpr[RCX] = if self.cpu.mode == CpuMode::X86_64 {
            PEB_ADDR
        } else {
            0
        };
        self.run_dll_inits(idx)?;
        self.run_tls_process_attach_callbacks(idx)
            .map_err(|error| {
                format!("{}: TLS process-attach callback failed: {error:?}", m.name)
            })?;
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
        if self.cpu.mode == CpuMode::X86_32 {
            m.write_u32(TEB_ADDR, u32::MAX).unwrap(); // NT_TIB.ExceptionList
            m.write_u32(PEB_ADDR + 0x0c, LDR_ADDR as u32).unwrap();
            m.write_u32(PEB_ADDR + 0x10, PARAMS_ADDR as u32).unwrap();
            m.write_u32(PEB_ADDR + 0x18, self.heap.process_heap as u32)
                .unwrap();
            m.write_u32(PEB_ADDR + 0x64, 1).unwrap(); // NumberOfProcessors
            m.write_u32(PEB_ADDR + 0xa4, 10).unwrap(); // Windows 10.0.19045
            m.write_u32(PEB_ADDR + 0xa8, 0).unwrap();
            m.write_u16(PEB_ADDR + 0xac, 19045).unwrap();
            m.write_u32(PEB_ADDR + 0xb0, 2).unwrap(); // VER_PLATFORM_WIN32_NT
            m.write_u32(TEB_ADDR + 0x18, TEB_ADDR as u32).unwrap();
            m.write_u32(TEB_ADDR + 0x20, 0x1000).unwrap(); // ClientId.UniqueProcess
            m.write_u32(TEB_ADDR + 0x24, 0x1004).unwrap(); // ClientId.UniqueThread
            m.write_u32(TEB_ADDR + 0x2c, 0).unwrap(); // ThreadLocalStoragePointer
            m.write_u32(TEB_ADDR + 0x30, PEB_ADDR as u32).unwrap();
            m.write_u32(TEB_ADDR + 0x34, 0).unwrap(); // LastErrorValue
            m.write_u32(LDR_ADDR, 0x30).unwrap();
            m.write_u8(LDR_ADDR + 4, 1).unwrap();
            for off in [0x0cu64, 0x14, 0x1c] {
                m.write_u32(LDR_ADDR + off, (LDR_ADDR + off) as u32)
                    .unwrap();
                m.write_u32(LDR_ADDR + off + 4, (LDR_ADDR + off) as u32)
                    .unwrap();
            }
            m.write_u32(PARAMS_ADDR, 0x1000).unwrap();
            m.write_u32(PARAMS_ADDR + 4, 0x400).unwrap();
        } else {
            m.write_u64(TEB_ADDR, u64::MAX).unwrap(); // NT_TIB.ExceptionList
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
            m.write_u32(LDR_ADDR, 0x58).unwrap();
            m.write_u8(LDR_ADDR + 4, 1).unwrap();
            for off in [0x10u64, 0x20, 0x30] {
                m.write_u64(LDR_ADDR + off, LDR_ADDR + off).unwrap();
                m.write_u64(LDR_ADDR + off + 8, LDR_ADDR + off).unwrap();
            }
            m.write_u32(PARAMS_ADDR, 0x1000).unwrap();
            m.write_u32(PARAMS_ADDR + 4, 0x400).unwrap();
            m.write_u64(PEB_ADDR + 0x30, self.heap.process_heap)
                .unwrap();
        }
        self.finish_params();
    }

    fn finish_params(&mut self) {
        let img = self.image_path.clone();
        let cmd = self.cmdline.clone();
        let (strs, handles): ([(u64, &str); 3], [u64; 3]) = if self.cpu.mode == CpuMode::X86_32 {
            (
                [(0x38, &img), (0x40, &cmd), (0x28, "C:\\")],
                [0x18, 0x1c, 0x20],
            )
        } else {
            (
                [(0x60, &img), (0x70, &cmd), (0x38, "C:\\")],
                [0x20, 0x28, 0x30],
            )
        };
        for (i, (off, s)) in strs.iter().enumerate() {
            let buf = PARAMS_ADDR + 0x400 + (i as u64) * 0x400;
            let _ = self.write_wstr(buf, s);
            let bytes = (s.encode_utf16().count() * 2) as u16;
            let _ = self.mem.write_u16(PARAMS_ADDR + off, bytes);
            let _ = self.mem.write_u16(PARAMS_ADDR + off + 2, bytes + 2);
            if self.cpu.mode == CpuMode::X86_32 {
                let _ = self.mem.write_u32(PARAMS_ADDR + off + 4, buf as u32);
            } else {
                let _ = self.mem.write_u64(PARAMS_ADDR + off + 8, buf);
            }
        }
        // Standard handles (pseudo handles recognised by our HLE).
        for (offset, handle) in handles.into_iter().zip([0x10u32, 0x14, 0x18]) {
            if self.cpu.mode == CpuMode::X86_32 {
                let _ = self.mem.write_u32(PARAMS_ADDR + offset, handle);
            } else {
                let _ = self.mem.write_u64(PARAMS_ADDR + offset, handle as u64);
            }
        }
    }

    /// Map a PE64 image. Returns the module index.
    pub fn load_image(
        &mut self,
        data: &[u8],
        path: &str,
        as_dll_hint: bool,
    ) -> Result<usize, String> {
        let pe = Pe::parse(data).map_err(|e| format!("{path}: {e}"))?;
        let supported_mode = match (pe.is_64, pe.machine) {
            (true, anywasm_pe::MACHINE_AMD64) => CpuMode::X86_64,
            (false, anywasm_pe::MACHINE_I386) => CpuMode::X86_32,
            _ => {
                return Err(format!(
                    "{path}: unsupported PE machine/magic combination (machine {:#x}, PE32+={})",
                    pe.machine, pe.is_64
                ));
            }
        };
        if supported_mode != self.cpu.mode {
            return Err(format!(
                "{path}: image bitness does not match process mode ({:?})",
                self.cpu.mode,
            ));
        }
        let runtime_functions = pe
            .runtime_functions()
            .map_err(|error| format!("{path}: {error}"))?;
        let tls_callback_vas = pe
            .tls_callbacks()
            .map_err(|error| format!("{path}: {error}"))?;
        let exception_table_rva = pe.dirs[anywasm_pe::DIR_EXCEPTION].rva;
        let resource_rva = pe.dirs[anywasm_pe::DIR_RESOURCE].rva;
        let resource_size = pe.dirs[anywasm_pe::DIR_RESOURCE].size;
        let size = align_up(pe.size_of_image as u64, 0x1000);
        let base = if self.range_free(pe.image_base, size) {
            pe.image_base
        } else if pe.has_relocs() {
            self.mem
                .find_free(
                    if self.cpu.mode == CpuMode::X86_32 {
                        0x1000_0000
                    } else {
                        0x1_8000_0000
                    },
                    size + 0x1_0000,
                )
                .ok_or("no address space for image")?
        } else {
            return Err(format!(
                "{path}: preferred base {:#x} is taken and the image has no relocations",
                pe.image_base
            ));
        };
        let tls_callbacks = tls_callback_vas
            .into_iter()
            .map(|callback| {
                let rva = callback.checked_sub(pe.image_base).ok_or_else(|| {
                    format!("{path}: TLS callback address {callback:#x} is below image base")
                })?;
                if rva >= pe.size_of_image as u64 {
                    return Err(format!(
                        "{path}: TLS callback address {callback:#x} is outside the image"
                    ));
                }
                Ok(base + rva)
            })
            .collect::<Result<Vec<_>, String>>()?;
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
            tls_callbacks,
            exports,
            ordinals,
            exception_table_rva,
            runtime_functions,
            resource_rva,
            resource_size,
            hle_group: None,
        });
        self.add_ldr_entry(idx);
        // imports
        for imp in pe.all_imports().map_err(|e| e.to_string())? {
            let provider = self.resolve_dll(&imp.dll)?;
            let dependencies = self.module_dependencies.entry(idx).or_default();
            if !dependencies.contains(&provider) {
                dependencies.push(provider);
            }
            for e in &imp.entries {
                let addr = self.bind_import(provider, &imp.dll, &e.name)?;
                let iat = base + e.iat_rva as u64;
                if self.cpu.mode == CpuMode::X86_32 {
                    self.mem
                        .write_force(iat, &(addr as u32).to_le_bytes())
                        .map_err(|er| format!("{er:?}"))?;
                } else {
                    self.mem
                        .write_force(iat, &addr.to_le_bytes())
                        .map_err(|er| format!("{er:?}"))?;
                }
            }
        }
        // forwarded exports are resolved after imports
        for (n, fw) in fwd {
            if let Some((dll, func)) = fw.split_once('.') {
                if let Ok(p) = self.resolve_dll(dll) {
                    let dependencies = self.module_dependencies.entry(idx).or_default();
                    if !dependencies.contains(&p) {
                        dependencies.push(p);
                    }
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
        let full = slot + 0x80;
        let base_s = slot + 0x140;
        let _ = self.write_wstr(full, &m.path);
        let _ = self.write_wstr(base_s, &m.name);
        let fl = (m.path.encode_utf16().count() * 2) as u16;
        let bl = (m.name.encode_utf16().count() * 2) as u16;
        let lists = if self.cpu.mode == CpuMode::X86_32 {
            let _ = self.mem.write_u32(slot + 0x18, m.base as u32);
            let _ = self.mem.write_u32(slot + 0x1c, m.entry as u32);
            let _ = self.mem.write_u32(slot + 0x20, m.size as u32);
            let _ = self.mem.write_u16(slot + 0x24, fl);
            let _ = self.mem.write_u16(slot + 0x26, fl + 2);
            let _ = self.mem.write_u32(slot + 0x28, full as u32);
            let _ = self.mem.write_u16(slot + 0x2c, bl);
            let _ = self.mem.write_u16(slot + 0x2e, bl + 2);
            let _ = self.mem.write_u32(slot + 0x30, base_s as u32);
            vec![(0x0cu64, 0x00u64), (0x14, 0x08)]
        } else {
            let _ = self.mem.write_u64(slot + 0x30, m.base);
            let _ = self.mem.write_u64(slot + 0x38, m.entry);
            let _ = self.mem.write_u32(slot + 0x40, m.size as u32);
            let _ = self.mem.write_u16(slot + 0x48, fl);
            let _ = self.mem.write_u16(slot + 0x4a, fl + 2);
            let _ = self.mem.write_u64(slot + 0x50, full);
            let _ = self.mem.write_u16(slot + 0x58, bl);
            let _ = self.mem.write_u16(slot + 0x5a, bl + 2);
            let _ = self.mem.write_u64(slot + 0x60, base_s);
            vec![(0x10u64, 0x00u64), (0x20, 0x10)]
        };
        // Insert at the tail of the load-order and memory-order lists.
        for (head_off, link_off) in lists {
            let head = LDR_ADDR + head_off;
            let node = slot + link_off;
            if self.cpu.mode == CpuMode::X86_32 {
                let tail = self.mem.read_u32(head + 4).unwrap_or(head as u32) as u64;
                let _ = self.mem.write_u32(node, head as u32);
                let _ = self.mem.write_u32(node + 4, tail as u32);
                let _ = self.mem.write_u32(tail, node as u32);
                let _ = self.mem.write_u32(head + 4, node as u32);
            } else {
                let tail = self.mem.read_u64(head + 8).unwrap_or(head);
                let _ = self.mem.write_u64(node, head);
                let _ = self.mem.write_u64(node + 8, tail);
                let _ = self.mem.write_u64(tail, node);
                let _ = self.mem.write_u64(head + 8, node);
            }
        }
    }

    /// Decide which module satisfies imports from `dll`: a bundled DLL or a host library.
    fn resolve_dll(&mut self, dll: &str) -> Result<usize, String> {
        if let Some(i) = self.module_by_name(dll) {
            return Ok(i);
        }
        if canonical_dll(&base_name(dll)) == "dinput8" {
            return Ok(self.register_hle_module(dll, "dinput8"));
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
            let size = self
                .vfs
                .file_len(&f)
                .ok_or_else(|| format!("indexed DLL disappeared from VFS: {f}"))?;
            let size =
                usize::try_from(size).map_err(|_| format!("DLL is too large to load: {f}"))?;
            let data = self
                .vfs
                .read_range(&f, 0, size)
                .map_err(|error| format!("could not read DLL {f}: {error:?}"))?;
            return self.load_image(&data, &f, true);
        }
        // The Windows API-set CRT contracts are implemented by UCRT. When a package supplies
        // the real ucrtbase.dll, resolve those contracts to that one module so its locale and
        // other process-wide CRT state are shared across all API-set imports.
        if canonical_dll(&base_name(dll)).starts_with("api-ms-win-crt-") {
            if let Some(i) = self.module_by_name("ucrtbase.dll") {
                return Ok(i);
            }
            let ucrt = crate::vfs::absolute("ucrtbase.dll", &self.current_directory);
            let file = if self.vfs.exists(&ucrt) {
                Some(crate::vfs::norm(&ucrt))
            } else if self.vfs.exists("ucrtbase.dll") {
                Some(crate::vfs::norm("ucrtbase.dll"))
            } else {
                self.vfs.find_by_name("ucrtbase.dll").cloned()
            };
            if let Some(f) = file {
                let size = self
                    .vfs
                    .file_len(&f)
                    .ok_or_else(|| format!("indexed DLL disappeared from VFS: {f}"))?;
                let size =
                    usize::try_from(size).map_err(|_| format!("DLL is too large to load: {f}"))?;
                let data = self
                    .vfs
                    .read_range(&f, 0, size)
                    .map_err(|error| format!("could not read DLL {f}: {error:?}"))?;
                return self.load_image(&data, &f, true);
            }
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
                    return hle::crt::data_import(self, &name, size)
                        .map_err(|error| format!("could not initialize {dll}!{name}: {error:?}"));
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
        let (tls_vector_offset, pointer_size) = if self.cpu.mode == CpuMode::X86_32 {
            (0x2c, 4u64)
        } else {
            (0x58, 8u64)
        };
        let mut arr = if pointer_size == 4 {
            self.mem.read_u32(TEB_ADDR + tls_vector_offset).unwrap_or(0) as u64
        } else {
            self.mem.read_u64(TEB_ADDR + tls_vector_offset).unwrap_or(0)
        };
        if arr == 0 {
            arr = self.alloc_virtual(0x1000, PROT_RW);
            if pointer_size == 4 {
                self.mem
                    .write_u32(TEB_ADDR + tls_vector_offset, arr as u32)
                    .unwrap();
            } else {
                self.mem
                    .write_u64(TEB_ADDR + tls_vector_offset, arr)
                    .unwrap();
            }
        }
        let idx = self.static_tls_next;
        let max_entries = 0x1000 / pointer_size;
        if idx as u64 >= max_entries {
            return Err(format!(
                "static TLS module index exceeds the supported {max_entries}-entry vector"
            ));
        }
        self.static_tls_next += 1;
        let block = self.alloc_virtual(align_up(total.max(16), 0x1000), PROT_RW);
        if !data.is_empty() {
            self.mem
                .write_force(block, &data)
                .map_err(|e| format!("{e:?}"))?;
        }
        let block_pointer = if pointer_size == 4 {
            (block as u32).to_le_bytes().to_vec()
        } else {
            block.to_le_bytes().to_vec()
        };
        self.mem
            .write_force(arr + idx as u64 * pointer_size, &block_pointer)
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
        let vector = self.try_alloc_virtual(0x1000, PROT_RW).ok_or_else(|| {
            Stop::Crash("guest allocation failed while allocating per-thread TLS vector".into())
        })?;
        for template in self.static_tls_templates.clone() {
            let block = self
                .try_alloc_virtual(align_up(template.total_size.max(16), 0x1000), PROT_RW)
                .ok_or_else(|| {
                    Stop::Crash("guest allocation failed while allocating per-thread TLS block".into())
                })?;
            if !template.data.is_empty() {
                self.mem
                    .write_force(block, &template.data)
                    .map_err(|e| Stop::Crash(format!("static TLS template copy failed: {e:?}")))?;
            }
            self.wrptr(
                vector
                    + template.index as u64
                        * if self.cpu.mode == CpuMode::X86_32 {
                            4
                        } else {
                            8
                        },
                block,
            )?;
        }
        Ok(vector)
    }

    /// Reserve and map `size` bytes of anonymous virtual memory (64 KiB granularity).
    pub fn try_alloc_virtual(&mut self, size: u64, prot: u8) -> Option<u64> {
        let size = align_up(size.max(1), 0x1000);
        let addr = self.mem.find_free(self.vmem_next, size)?;
        if !self.mem.map(addr, size, prot) {
            return None;
        }
        self.vmem.insert(addr, (size, prot));
        self.vmem_next = align_up(addr + size, 0x1_0000);
        Some(addr)
    }

    /// Reserve and map `size` bytes of anonymous virtual memory (64 KiB granularity).
    pub fn alloc_virtual(&mut self, size: u64, prot: u8) -> u64 {
        self.try_alloc_virtual(size, prot)
            .expect("address space exhausted")
    }

    /// Run DllMain(DLL_PROCESS_ATTACH) for bundled DLLs in dependency-first order.
    fn run_dll_inits(&mut self, exe_idx: usize) -> Result<(), String> {
        for i in self.dll_init_order(exe_idx) {
            let name = self.modules[i].name.clone();
            let r = self.run_dll_process_attach(i);
            match r {
                Ok(0) => return Err(format!("{name}: DllMain returned FALSE")),
                Ok(_) => {}
                Err(e) => return Err(format!("{name}: DllMain failed: {e:?}")),
            }
        }
        Ok(())
    }

    /// Run TLS callbacks for DLL_PROCESS_ATTACH in the order stored by the PE image.
    fn run_tls_process_attach_callbacks(&mut self, index: usize) -> Result<(), Stop> {
        let Some(module) = self.modules.get(index) else {
            return Err(Stop::Crash(format!(
                "invalid module index {index} for TLS callbacks"
            )));
        };
        let (base, callbacks) = (module.base, module.tls_callbacks.clone());
        for callback in callbacks {
            let _ = self.call_guest(callback, &[base, 1, 0])?;
        }
        Ok(())
    }

    /// Return the not-yet-initialized bundled DLLs reachable from `root`, dependencies first.
    pub(crate) fn dll_init_order(&self, root: usize) -> Vec<usize> {
        fn visit(
            process: &Process,
            index: usize,
            visited: &mut HashSet<usize>,
            order: &mut Vec<usize>,
        ) {
            if !visited.insert(index) {
                return;
            }
            if let Some(dependencies) = process.module_dependencies.get(&index) {
                for &dependency in dependencies {
                    visit(process, dependency, visited, order);
                }
            }
            let Some(module) = process.modules.get(index) else {
                return;
            };
            if module.is_dll
                && (module.entry != 0 || !module.tls_callbacks.is_empty())
                && module.hle_group.is_none()
                && !process.initialized_dlls.contains(&index)
                && !process.initializing_dlls.contains(&index)
            {
                order.push(index);
            }
        }

        let mut visited = HashSet::new();
        let mut order = Vec::new();
        visit(self, root, &mut visited, &mut order);
        order
    }

    /// Invoke TLS callbacks and DllMain(DLL_PROCESS_ATTACH) once, preventing recursive re-entry.
    pub(crate) fn run_dll_process_attach(&mut self, index: usize) -> Result<u64, Stop> {
        let Some(module) = self.modules.get(index) else {
            return Err(Stop::Crash(format!("invalid DLL module index {index}")));
        };
        let (base, entry) = (module.base, module.entry);
        self.initializing_dlls.insert(index);
        let result = self.run_tls_process_attach_callbacks(index).and_then(|()| {
            if entry == 0 {
                Ok(1)
            } else {
                self.call_guest(entry, &[base, 1, 0])
            }
        });
        self.initializing_dlls.remove(&index);
        if matches!(result, Ok(value) if value != 0) {
            self.initialized_dlls.insert(index);
        }
        result
    }

    // ── execution ────────────────────────────────────────────────────────────

    /// Call a guest function with integer args and run until it returns. Re-entrant.
    pub fn call_guest(&mut self, addr: u64, args: &[u64]) -> Result<u64, Stop> {
        let saved = self.cpu.clone();
        if self.cpu.mode == CpuMode::X86_32 {
            let sp = (self.cpu.gpr[RSP].wrapping_sub((args.len() as u64 + 1) * 4)) & !3;
            self.wr32(sp, RET_MAGIC as u32)?;
            for (index, argument) in args.iter().enumerate() {
                self.wr32(sp + 4 + index as u64 * 4, *argument as u32)?;
            }
            self.cpu.gpr[RSP] = sp;
            self.cpu.rip = addr as u32 as u64;
            self.cpu.fs_base = TEB_ADDR;
            self.nested += 1;
            let r = self.run_until_magic();
            self.nested -= 1;
            let ret = self.cpu.gpr[RAX] as u32 as u64;
            let keep_exit = self.exit_code;
            self.cpu = saved;
            return match r {
                Ok(()) => Ok(ret),
                Err(Stop::StepLimit) => Err(Stop::Crash(CALLBACK_BUDGET_EXHAUSTED.into())),
                Err(Stop::Exit(c)) => {
                    self.exit_code = keep_exit.or(Some(c));
                    Err(Stop::Exit(c))
                }
                Err(e) => Err(e),
            };
        }
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
            Err(Stop::StepLimit) => Err(Stop::Crash(CALLBACK_BUDGET_EXHAUSTED.into())),
            Err(Stop::Exit(c)) => {
                self.exit_code = keep_exit.or(Some(c));
                Err(Stop::Exit(c))
            }
            Err(e) => Err(e),
        }
    }

    fn run_until_magic(&mut self) -> Result<(), Stop> {
        let mut recent = VecDeque::with_capacity(128);
        loop {
            if self.cpu.rip == RET_MAGIC {
                return Ok(());
            }
            if let Err(stop) = self.ensure_run_budget(1) {
                #[cfg(target_arch = "wasm32")]
                if matches!(stop, Stop::StepLimit) {
                    // The Asyncify-enabled browser runtime unwinds the Rust stack here, lets the
                    // worker return to its event loop, and resumes this exact HLE call on the
                    // next run slice. The import returns that slice's budget after rewind.
                    let next_budget = unsafe { yield_slice() };
                    if next_budget == 0 {
                        return Err(Stop::Crash(
                            "callback resumed without a worker slice budget".into(),
                        ));
                    }
                    self.active_run_budget = Some(next_budget);
                    continue;
                }
                return Err(stop);
            }
            if self.trace_imports {
                if recent.len() == 128 {
                    recent.pop_front();
                }
                recent.push_back(trace_guest_instruction(&self.cpu, &self.mem));
            }
            match self.step_once() {
                Ok(None) => {}
                Ok(Some(stop)) => return Err(stop),
                Err(Stop::Crash(message)) if !recent.is_empty() => {
                    let trace = recent.into_iter().collect::<Vec<_>>().join("\n");
                    let arch = if self.cpu.mode == CpuMode::X86_32 {
                        "x86"
                    } else {
                        "x64"
                    };
                    return Err(Stop::Crash(format!(
                        "{message}\nrecent {arch} instructions:\n{trace}"
                    )));
                }
                Err(stop) => return Err(stop),
            }
        }
    }

    /// Execute one instruction or one HLE call. `Ok(Some(stop))` only for terminal conditions.
    fn step_once(&mut self) -> Result<Option<Stop>, Stop> {
        let rip = self.cpu.rip;
        if rip >= THUNK_BASE && rip < THUNK_BASE + THUNK_BYTES {
            self.charge_retired(1)?;
            self.dispatch_thunk(rip)?;
            return Ok(self.yield_request.take());
        }
        match self.dcache.step(&mut self.cpu, &mut self.mem) {
            Ok(()) => {
                self.charge_retired(1)?;
                Ok(None)
            }
            Err(e) => {
                if crate::exception::dispatch_cpu_exception(self, &e)? {
                    Ok(None)
                } else {
                    Err(self.describe_exit(e))
                }
            }
        }
    }

    fn ensure_run_budget(&self, count: u64) -> Result<(), Stop> {
        if self
            .active_run_budget
            .is_some_and(|remaining| count > remaining)
        {
            Err(Stop::StepLimit)
        } else {
            Ok(())
        }
    }

    fn charge_retired(&mut self, count: u64) -> Result<(), Stop> {
        self.ensure_run_budget(count)?;
        if let Some(remaining) = self.active_run_budget.as_mut() {
            *remaining -= count;
            self.quantum_used = self.quantum_used.saturating_add(count);
        }
        self.retired_instructions = self.retired_instructions.saturating_add(count);
        Ok(())
    }

    fn execute_slice(&mut self, max_instructions: u64) -> Result<(u64, Option<Stop>), Stop> {
        let rip = self.cpu.rip;
        if rip >= THUNK_BASE && rip < THUNK_BASE + THUNK_BYTES {
            self.charge_retired(1)?;
            self.dispatch_thunk(rip)?;
            return Ok((1, self.yield_request.take()));
        }
        if self.trace_gta_eventvol && self.cpu.mode == CpuMode::X86_32 && rip as u32 == 0x0082_4733
        {
            let frame = self.cpu.gpr[RBP] as u32 as u64;
            let locals = [-4i32, -20, -24, -28, -32, -112].map(|offset| {
                self.mem
                    .read_u32((frame as i64 + offset as i64) as u64)
                    .unwrap_or(0)
            });
            self.stderr.extend_from_slice(
                format!(
                    "[gta-startup-return] eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esi={:#x} edi={:#x} ebp={frame:#x} locals={locals:08x?}\n",
                    self.cpu.gpr[RAX] as u32,
                    self.cpu.gpr[RBX] as u32,
                    self.cpu.gpr[RCX] as u32,
                    self.cpu.gpr[RDX] as u32,
                    self.cpu.gpr[RSI] as u32,
                    self.cpu.gpr[RDI] as u32,
                )
                .as_bytes(),
            );
        }
        let gta_eventvol_trace = self.trace_gta_eventvol
            && self.cpu.mode == CpuMode::X86_32
            && ((0x4d9930..=0x4d9c0e).contains(&(rip as u32 as u64))
                || (0x5b9c60..=0x5b9e40).contains(&(rip as u32 as u64))
                || (0x5bfaf4..=0x5bfb04).contains(&(rip as u32 as u64))
                || (0x507d93..=0x507da5).contains(&(rip as u32 as u64))
                || (0x4f2fb0..=0x4f2fd0).contains(&(rip as u32 as u64))
                || (0x502bf0..=0x502c10).contains(&(rip as u32 as u64))
                || (0x4e7850..=0x4e7860).contains(&(rip as u32 as u64))
                || (0x4dd5a3..=0x4dd5a7).contains(&(rip as u32 as u64))
                || (0x5ba160..=0x5ba191).contains(&(rip as u32 as u64))
                || (0x5d5ac0..=0x5d5acb).contains(&(rip as u32 as u64))
                || (0x5da020..=0x5da05d).contains(&(rip as u32 as u64))
                || (0x5d7ec0..=0x5d7ed5).contains(&(rip as u32 as u64)));
        if gta_eventvol_trace {
            let current = rip as u32 as u64;
            if matches!(
                current,
                0x4d9950
                    | 0x4d9955
                    | 0x4d997a
                    | 0x4d9980
                    | 0x4d9985
                    | 0x4d998c
                    | 0x4d999a
                    | 0x4d99a3
                    | 0x4d99b3
                    | 0x4d99b8
                    | 0x4d99bc
                    | 0x4d99c2
                    | 0x4d99c7
                    | 0x4d99c9
                    | 0x4d99cb
                    | 0x4d99df
                    | 0x4d99e1
                    | 0x4d99f0
                    | 0x4d99f6
                    | 0x4d99f8
                    | 0x4d9a0f
                    | 0x4d9a18
                    | 0x4d9a22
                    | 0x4d9a25
                    | 0x4d9a32
                    | 0x4d9a36
                    | 0x4d9a39
                    | 0x4d9a3b
                    | 0x4d9a46
                    | 0x4d9a4d
                    | 0x4d9a4f
                    | 0x4d9a51
                    | 0x4d9a5e
                    | 0x4d9a61
                    | 0x4d9a6e
                    | 0x4d9a74
                    | 0x4d9a8e
                    | 0x4d9a9a
                    | 0x4d9aa5
                    | 0x4d9ab4
                    | 0x4d9ace
                    | 0x4d9ae3
                    | 0x4d9af1
                    | 0x4d9b47
                    | 0x4d9b51
                    | 0x4d9b53
                    | 0x4d9b66
                    | 0x4d9b76
                    | 0x4d9bb0
                    | 0x4d9bbe
                    | 0x4d9bc4
                    | 0x4d9bd4
                    | 0x4d9c01
                    | 0x5bfaf9
                    | 0x5bfafe
                    | 0x5bfb00
                    | 0x5b9c60
                    | 0x5b9c72
                    | 0x5b9cd2
                    | 0x5b9d50
                    | 0x5b9d55
                    | 0x5b9d59
                    | 0x5b9d63
                    | 0x5b9d68
                    | 0x5b9d72
                    | 0x5b9d77
                    | 0x5b9d7c
                    | 0x5b9d80
                    | 0x5b9d8d
                    | 0x5b9d92
                    | 0x5b9d95
                    | 0x5b9d9a
                    | 0x5b9da5
                    | 0x5b9dab
                    | 0x5b9db3
                    | 0x5b9dd5
                    | 0x507d93
                    | 0x507d99
                    | 0x507da1
                    | 0x4dd5a3
                    | 0x5ba160
                    | 0x5ba171
                    | 0x5ba175
                    | 0x5ba17a
                    | 0x5ba17e
                    | 0x5ba183
                    | 0x5ba185
                    | 0x5ba187
                    | 0x5ba18a
                    | 0x5d5ac0
                    | 0x5d5ac5
                    | 0x5d5acb
                    | 0x5da020
                    | 0x5da03b
                    | 0x5da044
                    | 0x5da049
                    | 0x5da04d
                    | 0x5da05d
                    | 0x5d7ec0
                    | 0x5d7ec5
                    | 0x5d7ec7
                    | 0x5d7eca
                    | 0x5d7ecf
                    | 0x5d7ed5
            ) || (0x4f2fb0..=0x4f2fd0).contains(&current)
                || (0x502bf0..=0x502c10).contains(&current)
                || (0x4e7850..=0x4e7860).contains(&current)
            {
                let buffer = self.mem.read_u32(0xbd00f8).unwrap_or(0);
                let call_slot = match current {
                    0x4d99e1 => 0x858350,
                    0x4d99f0 => 0x85801c,
                    _ => 0,
                };
                let call_target = if call_slot != 0 {
                    self.mem.read_u32(call_slot).unwrap_or(0)
                } else {
                    0
                };
                let call_target_name = self
                    .thunk_index
                    .iter()
                    .find_map(|((group, name), address)| {
                        (*address == call_target as u64).then(|| format!("{group}!{name}"))
                    })
                    .unwrap_or_default();
                let call_target_module = if call_target != 0 {
                    self.module_for(call_target as u64).unwrap_or("?")
                } else {
                    "?"
                };
                let stack = (0..5)
                    .map(|index| {
                        self.mem
                            .read_u32(self.cpu.gpr[RSP].wrapping_add(index * 4))
                            .unwrap_or(0)
                    })
                    .collect::<Vec<_>>();
                let ecx = self.cpu.gpr[RCX] as u32 as u64;
                let ecx_words = [
                    self.mem.read_u32(ecx).unwrap_or(0),
                    self.mem.read_u32(ecx.wrapping_add(4)).unwrap_or(0),
                ];
                self.stderr.extend_from_slice(
                    format!(
                        "[gta-eventvol] retired={} rip={current:#x} eax={:#x} ebx={:#x} ecx={ecx:#x} edx={:#x} esi={:#x} edi={:#x} global[bd00f8]={buffer:#x} ecx_words={ecx_words:08x?} call_target={call_target:#x} module={call_target_module} {call_target_name} stack={stack:08x?}\n",
                        self.retired_instructions,
                        self.cpu.gpr[RAX] as u32,
                        self.cpu.gpr[RBX] as u32,
                        self.cpu.gpr[RDX] as u32,
                        self.cpu.gpr[RSI] as u32,
                        self.cpu.gpr[RDI] as u32,
                    )
                    .as_bytes(),
                );
            }
            return match self.step_once() {
                Ok(Some(stop)) => Ok((1, Some(stop))),
                Ok(None) => Ok((1, None)),
                Err(stop) => Err(stop),
            };
        }
        let gta_format_trace = self.trace_modules_only
            && self.cpu.mode == CpuMode::X86_32
            && (((0x4cc5c0..=0x4cc949).contains(&(rip as u32 as u64)))
                || ((0x4cce60..=0x4cd025).contains(&(rip as u32 as u64))));
        if gta_format_trace {
            let current = rip as u32 as u64;
            let bytes = (0..8)
                .map(|offset| self.mem.read_u8(current + offset).ok())
                .take_while(Option::is_some)
                .flatten()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let stack = (0..6)
                .map(|index| {
                    self.mem
                        .read_u32(self.cpu.gpr[RSP].wrapping_add(index * 4))
                        .unwrap_or(0)
                })
                .collect::<Vec<_>>();
            self.stderr.extend_from_slice(
                format!(
                    "[gta-format-step] rip={current:#x} eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esi={:#x} edi={:#x} flags={:#x} stack={stack:08x?} code=[{bytes}]\n",
                    self.cpu.gpr[RAX] as u32,
                    self.cpu.gpr[RBX] as u32,
                    self.cpu.gpr[RCX] as u32,
                    self.cpu.gpr[RDX] as u32,
                    self.cpu.gpr[RSI] as u32,
                    self.cpu.gpr[RDI] as u32,
                    self.cpu.rflags as u32,
                )
                .as_bytes(),
            );
            return match self.step_once() {
                Ok(Some(stop)) => Ok((1, Some(stop))),
                Ok(None) => Ok((1, None)),
                Err(stop) => Err(stop),
            };
        }
        if self.trace_modules_only
            && self.cpu.mode == CpuMode::X86_32
            && matches!(rip as u32 as u64, 0x7fb230 | 0x7fb261 | 0x7fb297)
        {
            let backend = self.mem.read_u32(0xc97b24).unwrap_or(0) as u64;
            let callback = self.mem.read_u32(backend.wrapping_add(0x58)).unwrap_or(0);
            let object = self.cpu.gpr[RSI] as u32 as u64;
            let object4 = self.mem.read_u32(object.wrapping_add(4)).unwrap_or(0);
            self.stderr.extend_from_slice(
                format!(
                    "[gta-render-object] rip={rip:#x} eax={:#x} esi={object:#x} callback={callback:#x} object4={object4:#x} args={:#x}/{:#x}/{:#x}/{:#x} stackret={:#x}\n",
                    self.cpu.gpr[RAX] as u32,
                    self.mem.read_u32(self.cpu.gpr[RSP].wrapping_add(4)).unwrap_or(0),
                    self.mem.read_u32(self.cpu.gpr[RSP].wrapping_add(8)).unwrap_or(0),
                    self.mem.read_u32(self.cpu.gpr[RSP].wrapping_add(12)).unwrap_or(0),
                    self.mem.read_u32(self.cpu.gpr[RSP].wrapping_add(16)).unwrap_or(0),
                    self.mem.read_u32(self.cpu.gpr[RSP]).unwrap_or(0),
                )
                .as_bytes(),
            );
        }
        if self.trace_modules_only
            && self.cpu.mode == CpuMode::X86_32
            && matches!(
                rip as u32 as u64,
                0x4cce60
                    | 0x4ccd70
                    | 0x4ccf21
                    | 0x4ccf39
                    | 0x4ccfc4
                    | 0x4cce48
                    | 0x4cc5c0
                    | 0x4cc945
                    | 0x4cc79a
                    | 0x4cc85c
                    | 0x4cc88a
                    | 0x4cc8b9
                    | 0x4cc91f
                    | 0x4cc943
            )
        {
            let object = self.cpu.gpr[RDI] as u32 as u64;
            let fields = [0xc, 0x10, 0x14, 0x18, 0x20, 0x21]
                .map(|offset| self.mem.read_u32(object.wrapping_add(offset)).unwrap_or(0));
            let caps = [0xc9bef4, 0xc9befc, 0xc9c048, 0xc9c050, 0xc9c054]
                .map(|address| self.mem.read_u32(address).unwrap_or(0));
            self.stderr.extend_from_slice(
                format!(
                    "[gta-surface-callback] rip={rip:#x} eax={:#x} ebx={:#x} ecx={:#x} esi={:#x} edi={object:#x} fields={fields:08x?} caps={caps:08x?} stackret={:#x}\n",
                    self.cpu.gpr[RAX] as u32,
                    self.cpu.gpr[RBX] as u32,
                    self.cpu.gpr[RCX] as u32,
                    self.cpu.gpr[RSI] as u32,
                    self.mem.read_u32(self.cpu.gpr[RSP]).unwrap_or(0),
                )
                .as_bytes(),
            );
        }
        if self.trace_modules_only
            && self.cpu.mode == CpuMode::X86_32
            && matches!(
                rip as u32 as u64,
                0x72f420 | 0x72f42a | 0x801cf8 | 0x801cfe | 0x7fb255
            )
        {
            let backend = self.mem.read_u32(0xc97b24).unwrap_or(0);
            let size_or_arg = self.mem.read_u32(self.cpu.gpr[RSP] + 4).unwrap_or(0);
            self.stderr.extend_from_slice(
                format!(
                    "[gta-backend-allocation] rip={rip:#x} eax={:#x} ecx={:#x} edx={:#x} esi={:#x} stackarg={size_or_arg:#x} backend={backend:#x}\n",
                    self.cpu.gpr[RAX] as u32,
                    self.cpu.gpr[RCX] as u32,
                    self.cpu.gpr[RDX] as u32,
                    self.cpu.gpr[RSI] as u32,
                )
                .as_bytes(),
            );
        }
        if self.trace_modules_only
            && self.cpu.mode == CpuMode::X86_32
            && matches!(rip as u32 as u64, 0x619b71 | 0x74884c)
        {
            let caller_stack = self
                .mem
                .read_u32(self.cpu.gpr[RSP].wrapping_add(4))
                .unwrap_or(0);
            self.stderr.extend_from_slice(
                format!(
                    "[gta-event-dispatch] rip={rip:#x} eax={:#x} edi={:#x} stack_arg={caller_stack:#x}\n",
                    self.cpu.gpr[RAX] as u32,
                    self.cpu.gpr[RDI] as u32,
                )
                .as_bytes(),
            );
        }
        if self.trace_modules_only
            && self.cpu.mode == CpuMode::X86_32
            && matches!(
                rip as u32 as u64,
                0x5bd600
                    | 0x5bd62b
                    | 0x5bd656
                    | 0x5bd65c
                    | 0x5bd661
                    | 0x5bd666
                    | 0x5bd676
                    | 0x5bd681
                    | 0x5bd684
                    | 0x5bd68e
                    | 0x5bd71c
                    | 0x5bd721
                    | 0x5bd724
                    | 0x5bd726
                    | 0x5bd748
            )
        {
            let backend = self.mem.read_u32(0xc97b24).unwrap_or(0) as u64;
            let state = self.mem.read_u32(backend.wrapping_add(0x150)).unwrap_or(0);
            let backend_factory = self.mem.read_u32(backend.wrapping_add(0x144)).unwrap_or(0);
            let backend_create = self.mem.read_u32(backend.wrapping_add(0x134)).unwrap_or(0);
            let backend_release = self.mem.read_u32(backend.wrapping_add(0x148)).unwrap_or(0);
            let remaining = self.mem.read_u32(0xc97b20).unwrap_or(0);
            let object = self.cpu.gpr[RSI] as u32 as u64;
            let field4 = self.mem.read_u32(object.wrapping_add(4)).unwrap_or(0);
            let field60 = self.mem.read_u32(object.wrapping_add(0x60)).unwrap_or(0);
            let field64 = self.mem.read_u32(object.wrapping_add(0x64)).unwrap_or(0);
            let field60_vtable = self.mem.read_u32(field60 as u64).unwrap_or(0);
            let field64_vtable = self.mem.read_u32(field64 as u64).unwrap_or(0);
            self.stderr.extend_from_slice(
                format!(
                    "[gta-backend-init] rip={rip:#x} eax={:#x} ebx={:#x} esi={object:#x} edi={:#x} backend={backend:#x} state={state:#x} factory={backend_factory:#x} create={backend_create:#x} release={backend_release:#x} remaining={remaining:#x} field4={field4:#x} field60={field60:#x}/{field60_vtable:#x} field64={field64:#x}/{field64_vtable:#x}\n",
                    self.cpu.gpr[RAX] as u32,
                    self.cpu.gpr[RBX] as u32,
                    self.cpu.gpr[RDI] as u32,
                )
                .as_bytes(),
            );
        }
        if self.trace_imports
            && self.cpu.mode == CpuMode::X86_64
            && (0x1_4003_0700..0x1_4003_0b00).contains(&rip)
        {
            let (block_cache, cpu, mem, recent) = (
                &mut self.block_cache,
                &mut self.cpu,
                &mut self.mem,
                &mut self.trace_recent_guest,
            );
            let (executed, result) =
                block_cache.execute_counted_with_hook(cpu, mem, max_instructions, |cpu, mem| {
                    // Limit this diagnostic to Tutorial01's failing object traversal at +0x30987.
                    if !(0x1_4003_0800..0x1_4003_0a20).contains(&cpu.rip) {
                        return;
                    }
                    if recent.len() == 128 {
                        recent.pop_front();
                    }
                    let object = cpu.gpr[RDI];
                    let words = (0..8)
                        .map(|index| {
                            format!(
                                "{:#x}",
                                mem.read_u64(object.wrapping_add(index * 8)).unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    recent.push_back(format!(
                        "{} rdi_words=[{words}]",
                        trace_guest_instruction(cpu, mem)
                    ));
                });
            self.charge_retired(executed)?;
            return match result {
                Ok(()) => Ok((executed, None)),
                Err(error) => {
                    if crate::exception::dispatch_cpu_exception(self, &error)? {
                        // The faulting instruction did not retire. Count exception dispatch as
                        // slice progress for the caller's zero-progress guard; instruction and
                        // callback budgets remain charged only by retired guest instructions.
                        return Ok((executed.max(1), None));
                    }
                    match self.describe_exit(error) {
                        Stop::Crash(message) => {
                            let trace = self
                                .trace_recent_guest
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n");
                            Err(Stop::Crash(format!(
                                "{message}\nrecent x64 instructions:\n{trace}"
                            )))
                        }
                        stop => Err(stop),
                    }
                }
            };
        }
        if self.trace_imports && self.cpu.mode == CpuMode::X86_32 {
            let mut recent = VecDeque::with_capacity(128);
            let mut executed = 0;
            while executed < max_instructions {
                let current_rip = self.cpu.rip as u32 as u64;
                if self.trace_modules_only
                    && self.cpu.mode == CpuMode::X86_32
                    && ((0x4cc5c0..=0x4cc949).contains(&current_rip)
                        || (0x4cce60..=0x4cd025).contains(&current_rip))
                {
                    let bytes = (0..8)
                        .map(|offset| self.mem.read_u8(current_rip + offset).ok())
                        .take_while(Option::is_some)
                        .flatten()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let stack = (0..6)
                        .map(|index| {
                            self.mem
                                .read_u32(self.cpu.gpr[RSP].wrapping_add(index * 4))
                                .unwrap_or(0)
                        })
                        .collect::<Vec<_>>();
                    self.stderr.extend_from_slice(
                        format!(
                            "[gta-format-step] rip={current_rip:#x} eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esi={:#x} edi={:#x} flags={:#x} stack={stack:08x?} code=[{bytes}]\n",
                            self.cpu.gpr[RAX] as u32,
                            self.cpu.gpr[RBX] as u32,
                            self.cpu.gpr[RCX] as u32,
                            self.cpu.gpr[RDX] as u32,
                            self.cpu.gpr[RSI] as u32,
                            self.cpu.gpr[RDI] as u32,
                            self.cpu.rflags as u32,
                        )
                        .as_bytes(),
                    );
                }
                if recent.len() == 128 {
                    recent.pop_front();
                }
                recent.push_back(trace_guest_instruction(&self.cpu, &self.mem));
                match self.step_once() {
                    Ok(Some(stop)) => return Ok((executed + 1, Some(stop))),
                    Ok(None) => executed += 1,
                    Err(Stop::Crash(message)) => {
                        let trace = recent.into_iter().collect::<Vec<_>>().join("\n");
                        return Err(Stop::Crash(format!(
                            "{message}\nrecent x86 instructions:\n{trace}"
                        )));
                    }
                    Err(stop) => return Err(stop),
                }
            }
            return Ok((executed, None));
        }
        if self.trace_gta_eventvol && self.cpu.mode == CpuMode::X86_32 {
            let thread_id = self.current_thread_id;
            let mut gta_copy_trace = Vec::new();
            let mut gta_txd_trace = Vec::new();
            let mut trace_particle_txd = self.trace_particle_txd;
            let mut particle_texture_index = self.trace_particle_texture_index;
            let mut rep_before: Option<(u32, u32, u32)> = None;
            let tracked_heap_sizes = [0x3100_5030u64, 0x31b0_6d40, 0x31b0_7318]
                .map(|address| self.heap.size_of(address));
            let (executed, result) = {
                let (block_cache, cpu, mem, recent, watched_copy_word) = (
                    &mut self.block_cache,
                    &mut self.cpu,
                    &mut self.mem,
                    &mut self.trace_recent_x86_ip,
                    &mut self.trace_gta_copy_watch,
                );
                block_cache.execute_counted_with_hook(
                    cpu,
                    mem,
                    max_instructions,
                    |cpu, mem| {
                        let rip = cpu.rip as u32 as u64;
                        let sp = cpu.gpr[RSP] as u32 as u64;
                        if rip == 0x7320b0 {
                            let slot = mem.read_u32(sp + 4).unwrap_or(0);
                            let path_ptr = mem.read_u32(sp + 8).unwrap_or(0) as u64;
                            let mut path = Vec::new();
                            for offset in 0..96 {
                                let Ok(byte) = mem.read_u8(path_ptr + offset) else { break };
                                if byte == 0 { break; }
                                path.push(byte.to_ascii_lowercase());
                            }
                            trace_particle_txd = path.ends_with(b"particle.txd");
                            if trace_particle_txd {
                                gta_txd_trace.push(format!(
                                    "[gta-txd-trace] LoadTxd(file) entry slot={slot:#x} path={:?} thread={thread_id:#x}",
                                    String::from_utf8_lossy(&path),
                                ));
                            }
                        }
                        if trace_particle_txd {
                            if rip == 0x731dd0 || rip == 0x730fc0 {
                                let arg0 = mem.read_u32(sp + 4).unwrap_or(0);
                                let arg1 = mem.read_u32(sp + 8).unwrap_or(0);
                                gta_txd_trace.push(format!(
                                    "[gta-txd-trace] entry rip={rip:#x} arg0={arg0:#x} arg1={arg1:#x} eax={:#x} esi={:#x} edi={:#x}",
                                    cpu.gpr[RAX] as u32, cpu.gpr[RSI] as u32, cpu.gpr[RDI] as u32,
                                ));
                            } else if rip == 0x730e60 {
                                particle_texture_index += 1;
                                let factory = mem.read_u32(0xc97b24).unwrap_or(0) as u64;
                                let read_callback = mem.read_u32(factory + 0xb0).unwrap_or(0);
                                gta_txd_trace.push(format!(
                                    "[gta-txd-trace] native-texture #{particle_texture_index} stream={:#x} return={:#x} factory={factory:#x} readCallback={read_callback:#x}",
                                    mem.read_u32(sp + 4).unwrap_or(0), mem.read_u32(sp).unwrap_or(0),
                                ));
                            } else if rip == 0x730ed6 || rip == 0x730f51 || rip == 0x73102e || rip == 0x73104c || rip == 0x731e11 || rip == 0x731e28 {
                                let words = (0..4)
                                    .map(|index| mem.read_u32(sp + index * 4).unwrap_or(0))
                                    .map(|word| format!("{word:#x}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let factory = mem.read_u32(0xc97b24).unwrap_or(0) as u64;
                                let read_callback = mem.read_u32(factory + 0xb0).unwrap_or(0);
                                gta_txd_trace.push(format!(
                                    "[gta-txd-trace] step rip={rip:#x} eax={:#x} ebx={:#x} esi={:#x} edi={:#x} stack=[{words}] remaining={:#x} readCallback={read_callback:#x}",
                                    cpu.gpr[RAX] as u32, cpu.gpr[RBX] as u32, cpu.gpr[RSI] as u32, cpu.gpr[RDI] as u32,
                                    mem.read_u16(sp + 8).unwrap_or(0),
                                ));
                            } else if rip == 0x732106 {
                                gta_txd_trace.push(format!(
                                    "[gta-txd-trace] LoadTxd(file) return eax={:#x} particleNativeTextures={particle_texture_index}",
                                    cpu.gpr[RAX] as u32,
                                ));
                                trace_particle_txd = false;
                            }
                        }
                        if rip == 0x4dffb4 {
                            let esi = cpu.gpr[RSI] as u32;
                            let edi = cpu.gpr[RDI] as u32;
                            let count = cpu.gpr[RCX] as u32;
                            let offset = 0x30c040a4u32.wrapping_sub(edi);
                            let source_word = mem.read_u32(esi.wrapping_add(offset) as u64).unwrap_or(0);
                            let destination_word = mem.read_u32(edi.wrapping_add(offset) as u64).unwrap_or(0);
                            rep_before = Some((esi, edi, count));
                            gta_copy_trace.push(format!(
                                "[gta-copy-trace] rep-boundary before rip={rip:#x} esi={esi:#x} edi={edi:#x} ecx={count:#x} df={} watchOffset={offset:#x} sourceAtWatch={source_word:#x} destBefore={destination_word:#x}",
                                cpu.flag(DF),
                            ));
                        } else if rip == 0x4dffb6 {
                            if let Some((source_start, destination_start, count)) = rep_before.take() {
                                let offset = 0x30c040a4u32.wrapping_sub(destination_start);
                                let source_word = mem.read_u32(source_start.wrapping_add(offset) as u64).unwrap_or(0);
                                let destination_word = mem.read_u32(0x30c040a4).unwrap_or(0);
                                gta_copy_trace.push(format!(
                                    "[gta-copy-trace] rep-boundary after rip={rip:#x} esi={:#x} edi={:#x} ecx={:#x} count={count:#x} watchOffset={offset:#x} sourceAtWatch={source_word:#x} destAfter={destination_word:#x}",
                                    cpu.gpr[RSI] as u32,
                                    cpu.gpr[RDI] as u32,
                                    cpu.gpr[RCX] as u32,
                                ));
                            }
                        }
                        if let Ok(watched_value) = mem.read_u32(0x30c040a4) {
                            if *watched_copy_word != Some(watched_value) {
                                gta_copy_trace.push(format!(
                                    "[gta-copy-watch] thread={thread_id:#x} rip={rip:#x} value={watched_copy_word:?}->{watched_value:#x} {}",
                                    trace_guest_instruction(cpu, mem),
                                ));
                                *watched_copy_word = Some(watched_value);
                            }
                        }
                        if (0x4d86b0..=0x4d8750).contains(&rip)
                            || (0x4f07e0..=0x4f0800).contains(&rip)
                            || (0x4e0280..=0x4e0372).contains(&rip)
                            || matches!(rip, 0x4f0c40 | 0x4f0e67)
                        {
                            let stack = (0..8)
                                .map(|index| {
                                    mem.read_u32((cpu.gpr[RSP] as u32).wrapping_add(index * 4) as u64)
                                        .unwrap_or(0)
                                })
                                .map(|word| format!("{word:#x}"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            let memory = if rip == 0x4e0280 {
                                let object = cpu.gpr[RCX] as u32 as u64;
                                let words = [0, 4, 0x14, 0x1c, 0x66c]
                                    .map(|offset| mem.read_u32(object.wrapping_add(offset)).unwrap_or(0));
                                let table = words[0] as u64;
                                let first = (0..8)
                                    .map(|index| mem.read_u32(table.wrapping_add(0x60 + index * 4)).unwrap_or(0))
                                    .collect::<Vec<_>>();
                                format!(" object={object:#x} fields={words:08x?} record8={first:08x?}")
                            } else if matches!(rip, 0x4e0357 | 0x4e035b | 0x4e035e | 0x4e0366) {
                                let table = cpu.gpr[RDX] as u32 as u64;
                                let offset = cpu.gpr[RSI] as u32 as u64;
                                let element = table.wrapping_add(offset);
                                let field = table
                                    .wrapping_add(cpu.gpr[RDI] as u32 as u64)
                                    .wrapping_add(0x14);
                                let object = cpu.gpr[RCX] as u32 as u64;
                                let base = mem.read_u32(object.wrapping_add(0x1c)).unwrap_or(0);
                                format!(
                                    " table={table:#x} element={element:#x} elementValue={:#x} field={field:#x} fieldValue={:#x} objectBase={base:#x}",
                                    mem.read_u32(element).unwrap_or(0),
                                    mem.read_u32(field).unwrap_or(0),
                                )
                            } else if rip == 0x4e0361 {
                                format!(
                                    " dataBaseHeapSize={:?} readBufferHeapSize={:?} readInteriorHeapSize={:?} dataBasePageProt={:?} faultPageProt={:?}",
                                    tracked_heap_sizes[0],
                                    tracked_heap_sizes[1],
                                    tracked_heap_sizes[2],
                                    mem.prot_of(0x3100_5000),
                                    mem.prot_of(0x3f07_5000),
                                )
                            } else {
                                String::new()
                            };
                            gta_copy_trace.push(format!(
                                "[gta-copy-trace] thread={thread_id:#x} {} eax={:#x} ebx={:#x} ecx={:#x} edx={:#x} esi={:#x} edi={:#x} ebp={:#x} esp={:#x} flags={:#x} stack=[{stack}]{memory}",
                                trace_guest_instruction(cpu, mem),
                                cpu.gpr[RAX] as u32,
                                cpu.gpr[RBX] as u32,
                                cpu.gpr[RCX] as u32,
                                cpu.gpr[RDX] as u32,
                                cpu.gpr[RSI] as u32,
                                cpu.gpr[RDI] as u32,
                                cpu.gpr[RBP] as u32,
                                cpu.gpr[RSP] as u32,
                                cpu.rflags as u32,
                            ));
                        }
                        if recent.len() == 128 {
                            recent.pop_front();
                        }
                        recent.push_back((thread_id, cpu.rip as u32));
                    },
                )
            };
            self.trace_particle_txd = trace_particle_txd;
            self.trace_particle_texture_index = particle_texture_index;
            if !gta_txd_trace.is_empty() {
                self.stderr
                    .extend_from_slice(format!("{}\n", gta_txd_trace.join("\n")).as_bytes());
            }
            if !gta_copy_trace.is_empty() {
                self.stderr
                    .extend_from_slice(format!("{}\n", gta_copy_trace.join("\n")).as_bytes());
            }
            self.charge_retired(executed)?;
            return match result {
                Ok(()) => Ok((executed, None)),
                Err(error) => match self.describe_exit(error) {
                    Stop::Crash(message) => {
                        let trace = self
                            .trace_recent_x86_ip
                            .iter()
                            .map(|(thread_id, rip)| format!("{thread_id:#x}:{rip:#x}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        Err(Stop::Crash(format!(
                            "{message}\nrecent x86 instruction addresses (thread:rip):\n{trace}"
                        )))
                    }
                    stop => Err(stop),
                },
            };
        }
        let (executed, result) =
            self.block_cache
                .execute_counted(&mut self.cpu, &mut self.mem, max_instructions);
        self.charge_retired(executed)?;
        match result {
            Ok(()) => Ok((executed, None)),
            Err(e) => {
                if crate::exception::dispatch_cpu_exception(self, &e)? {
                    // As above, this is scheduler progress, not a retired guest instruction.
                    Ok((executed.max(1), None))
                } else {
                    Err(self.describe_exit(e))
                }
            }
        }
    }

    fn describe_exit(&self, e: Exit) -> Stop {
        let c = &self.cpu;
        let stack = if c.mode == CpuMode::X86_32 {
            let words = (0..8)
                .map(|index| {
                    self.rd32((c.gpr[RSP] as u32).wrapping_add(index * 4) as u64)
                        .unwrap_or(0)
                })
                .map(|word| format!("{word:#x}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" stack32=[{words}]")
        } else if self.trace_imports {
            let words = (0..8)
                .map(|index| self.rd64(c.gpr[RSP].saturating_add(index * 8)).unwrap_or(0))
                .map(|word| format!("{word:#x}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" stack=[{words}]")
        } else {
            String::new()
        };
        Stop::Crash(format!(
            "{:?} at rip={:#x} (module {}) rax={:#x} rcx={:#x} rdx={:#x} rsp={:#x} rflags={:#x}{stack}",
            e,
            c.rip,
            self.module_for(c.rip).unwrap_or("?"),
            c.gpr[RAX],
            c.gpr[RCX],
            c.gpr[RDX],
            c.gpr[RSP],
            c.rflags
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
        let x86_frame = if self.cpu.mode == CpuMode::X86_32 {
            let sp = self.cpu.gpr[RSP] as u32 as u64;
            let ra = self.rd32(sp)? as u64;
            Some((self.cpu.clone(), sp, ra))
        } else {
            None
        };
        if x86_frame.is_some() {
            for (index, register) in [RCX, RDX, R8, R9].into_iter().enumerate() {
                self.cpu.gpr[register] = self.arg(index) as u32 as u64;
            }
        }
        if self.trace_imports && !self.trace_modules_only {
            if self.cpu.mode == CpuMode::X86_32 {
                let sp = self.cpu.gpr[RSP] as u32 as u64;
                let caller = self.rd32(sp).unwrap_or(0);
                let args = (0..4)
                    .map(|index| format!("{:#x}", self.rd32(sp + 4 + index * 4).unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.stderr.extend_from_slice(
                    format!("[import32] {group}!{name} sp={sp:#x} ret={caller:#x} args=[{args}]\n")
                        .as_bytes(),
                );
            }
            let traced_args = match (group, name.as_str()) {
                ("kernel32", "HeapAlloc") => Some(3),
                ("kernel32", "TlsGetValue") => Some(1),
                ("kernel32", "GetModuleHandleA") => Some(1),
                ("kernel32", "GetProcAddress") => Some(2),
                ("kernel32", "GetLogicalProcessorInformationEx") => Some(3),
                _ => None,
            };
            let line = if let Some(arg_count) = traced_args {
                let args = (0..arg_count)
                    .map(|index| format!("{:#x}", self.arg(index)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let caller = self.rdptr(self.cpu.gpr[RSP]).unwrap_or(0);
                let module = self
                    .modules
                    .iter()
                    .find(|m| m.hle_group.is_none() && caller >= m.base && caller < m.base + m.size)
                    .map(|m| format!("{}+{:#x}", m.name, caller - m.base))
                    .unwrap_or_else(|| "?".to_string());
                format!(
                    "[import] {group}!{name} args=[{args}] caller={caller:#x} module={module}\n"
                )
            } else {
                format!("[import] {group}!{name}\n")
            };
            self.stderr.extend_from_slice(line.as_bytes());
        }
        if self.trace_modules_only && group == "com" {
            let args = (0..8)
                .map(|index| format!("{:#x}", self.arg(index)))
                .collect::<Vec<_>>()
                .join(", ");
            let caller = self.rdptr(self.cpu.gpr[RSP]).unwrap_or(0);
            let detail = if name == "IDirect3DDevice9::SetVertexDeclaration" {
                let declaration = self.arg(1);
                let bytes = self
                    .read_bytes(declaration, 32)
                    .unwrap_or_default()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    " decl={declaration:#x} known_com={} bytes=[{bytes}]",
                    self.extra.com.objs.contains_key(&declaration)
                )
            } else {
                String::new()
            };
            self.stderr.extend_from_slice(
                format!("[com-call] {name} args=[{args}]{detail} caller={caller:#x}\n").as_bytes(),
            );
        }
        let import_caller = self.rdptr(self.cpu.gpr[RSP]).unwrap_or(0);
        if self.trace_modules_only
            && group == "kernel32"
            && name == "GetLogicalProcessorInformationEx"
        {
            let args = (0..3)
                .map(|index| format!("{:#x}", self.arg(index)))
                .collect::<Vec<_>>()
                .join(", ");
            self.stderr.extend_from_slice(
                format!("[target-import] {group}!{name} args=[{args}] caller={import_caller:#x}\n")
                    .as_bytes(),
            );
        }
        if self.trace_modules_only
            && ((0x746900..0x746a10).contains(&import_caller)
                || (0x748700..0x748900).contains(&import_caller))
        {
            let args = (0..8)
                .map(|index| format!("{:#x}", self.arg(index)))
                .collect::<Vec<_>>()
                .join(", ");
            self.stderr.extend_from_slice(
                format!("[target-import] {group}!{name} args=[{args}] caller={import_caller:#x}\n")
                    .as_bytes(),
            );
        }
        if self.trace_modules_only
            && ((group == "user32"
                && matches!(
                    name.as_str(),
                    "CreateWindowExA"
                        | "CreateWindowExW"
                        | "ShowWindow"
                        | "GetMessageA"
                        | "GetMessageW"
                        | "PeekMessageA"
                        | "PeekMessageW"
                        | "DispatchMessageA"
                        | "PostQuitMessage"
                        | "DestroyWindow"
                        | "MessageBoxA"
                        | "DefWindowProcA"
                        | "DefWindowProcW"
                ))
                || (group == "kernel32"
                    && matches!(name.as_str(), "ExitProcess" | "TerminateProcess")))
        {
            let args = (0..8)
                .map(|index| format!("{:#x}", self.arg(index)))
                .collect::<Vec<_>>()
                .join(", ");
            let caller = self.rdptr(self.cpu.gpr[RSP]).unwrap_or(0);
            self.stderr.extend_from_slice(
                format!("[target-lifecycle] {group}!{name} args=[{args}] caller={caller:#x}\n")
                    .as_bytes(),
            );
        }
        let Some(f) = func else {
            self.unimplemented_log
                .push((group.to_string(), name.clone()));
            if self.trace_imports && !self.trace_modules_only {
                let args = (0..8)
                    .map(|index| format!("{:#x}", self.arg(index)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let caller = self.rdptr(self.cpu.gpr[RSP]).unwrap_or(0);
                self.stderr.extend_from_slice(
                    format!("[unimplemented] {group}!{name} args=[{args}] caller={caller:#x}\n")
                        .as_bytes(),
                );
            }
            return Err(Stop::Unimplemented {
                dll: group.to_string(),
                name,
            });
        };
        self.skip_ret = false;
        let d3d9_output = if group == "com" {
            match name.as_str() {
                "IDirect3D9::CreateDevice" => Some(self.arg(6)),
                "IDirect3DDevice9::CreateTexture" => Some(self.arg(7)),
                "IDirect3DDevice9::CreateDepthStencilSurface" => Some(self.arg(7)),
                "IDirect3DTexture9::LockRect" | "IDirect3DSurface9::LockRect" => Some(self.arg(3)),
                _ => None,
            }
        } else {
            None
        };
        let deferred_context = if group == "com" {
            name.split_once("::")
                .filter(|(iface, method)| {
                    iface.starts_with("ID3D11DeviceContext")
                        && !method.starts_with("Get")
                        && !matches!(
                            *method,
                            "AddRef"
                                | "Release"
                                | "QueryInterface"
                                | "Begin"
                                | "End"
                                | "ExecuteCommandList"
                                | "FinishCommandList"
                        )
                })
                .map(|_| self.arg(0))
                .filter(|context| self.extra.com.deferred_contexts.contains_key(context))
        } else {
            None
        };
        let ret = if let Some(context) = deferred_context {
            let deferred_commands = self
                .extra
                .com
                .deferred_contexts
                .get_mut(&context)
                .map(|state| std::mem::take(&mut state.commands))
                .unwrap_or_default();
            let immediate_commands = std::mem::replace(&mut self.extra.gpu.buf, deferred_commands);
            let initial_len = self.extra.gpu.buf.len();
            let result = f(self);
            let recorded_commands = std::mem::replace(&mut self.extra.gpu.buf, immediate_commands);
            if let Some(state) = self.extra.com.deferred_contexts.get_mut(&context) {
                retain_deferred_state_commands(state, &recorded_commands[initial_len..]);
                state.commands = recorded_commands;
            }
            result?
        } else {
            f(self)?
        };
        if self.trace_modules_only
            && group == "com"
            && (name.starts_with("IDirect3D9::")
                || name.starts_with("IDirect3DDevice9::")
                || name.starts_with("IDirect3DSurface9::"))
        {
            let output = d3d9_output
                .map(|address| {
                    format!(
                        " out={address:#x}:{:#x}",
                        self.mem.read_u32(address).unwrap_or(0)
                    )
                })
                .unwrap_or_default();
            self.stderr
                .extend_from_slice(format!("[com-result] {name} hr={ret:#x}{output}\n").as_bytes());
        }
        self.refresh_waiters();
        if self.skip_ret {
            self.skip_ret = false;
            return Ok(());
        }
        if let Some((saved_cpu, sp, ra)) = x86_frame {
            self.cpu = saved_cpu;
            self.cpu.gpr[RAX] = ret as u32 as u64;
            self.cpu.gpr[RSP] = sp + 4 + stdcall_stack_bytes(group, &name);
            self.cpu.rip = ra as u32 as u64;
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
        if self.trace_gta_eventvol {
            let sp = self.cpu.gpr[RSP];
            let stack = (0..8)
                .map(|index| self.mem.read_u32(sp.wrapping_add(index * 4)).unwrap_or(0))
                .collect::<Vec<_>>();
            let recent = self
                .trace_recent_x86_ip
                .iter()
                .map(|(thread, rip)| format!("{thread:#x}:{rip:#x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let mut frame = self.cpu.gpr[RBP] as u32 as u64;
            let mut frames = Vec::new();
            for _ in 0..16 {
                let previous = self.mem.read_u32(frame).unwrap_or(0) as u64;
                let return_address = self.mem.read_u32(frame + 4).unwrap_or(0);
                if frame < 0x1_0000 || return_address == 0 {
                    break;
                }
                let args = (0..4)
                    .map(|index| self.mem.read_u32(frame + 8 + index * 4).unwrap_or(0))
                    .collect::<Vec<_>>();
                frames.push(format!("{frame:#x}->{return_address:#x} args={args:08x?}"));
                if previous <= frame {
                    break;
                }
                frame = previous;
            }
            let frames = frames.join(" ");
            self.stderr.extend_from_slice(
                format!(
                    "[process-exit] code={code:#x} rip={:#x} thread={} guest={} eax={:#x} frames={frames} recent={recent} stack={stack:08x?}\n",
                    self.cpu.rip,
                    self.current_thread_id,
                    self.current_thread_is_guest,
                    self.cpu.gpr[RAX],
                )
                .as_bytes(),
            );
        }
        if self.exit_code.is_none() {
            while let Some(f) = self.atexit.pop() {
                self.call_guest(f, &[])?;
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
        if self.active_run_budget.is_some() {
            return Stop::Crash("Process::run cannot be re-entered from a guest callback".into());
        }
        self.active_run_budget = Some(max_steps);
        let result = self.run_with_budget();
        self.active_run_budget = None;
        result
    }

    fn run_with_budget(&mut self) -> RunResult {
        while self.active_run_budget.unwrap_or(0) != 0 {
            if let Some((kind, a, b, c)) = self.pending_input_events.pop_front() {
                if let Err(stop) = hle::user32::input_event(self, kind, a, b, c) {
                    return stop;
                }
                continue;
            }
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
            let slice = self
                .active_run_budget
                .unwrap_or(0)
                .min(THREAD_QUANTUM.saturating_sub(self.quantum_used).max(1));
            match self.execute_slice(slice) {
                Ok((_, Some(stop))) => return stop,
                Ok((executed, None)) => {
                    if executed == 0 {
                        return Stop::Crash(format!(
                            "block executor made no progress at rip={:#x}",
                            self.cpu.rip
                        ));
                    }
                }
                Err(Stop::ThreadExit(code)) if self.current_thread_is_guest => {
                    self.exit_current_guest_thread(code);
                }
                Err(Stop::ThreadExit(code)) => {
                    return Stop::Crash(format!(
                        "ExitThread used by non-thread context with code {code}"
                    ))
                }
                Err(Stop::GuestUnwind(_)) => {
                    return Stop::Crash(
                        "RtlUnwindEx transfer escaped its active guest exception dispatcher".into(),
                    )
                }
                Err(Stop::GuestCatch32(_)) => {
                    return Stop::Crash(
                        "PE32 C++ catch transfer escaped its active guest exception dispatcher".into(),
                    )
                }
                Err(stop) => return stop,
            }
        }
        Stop::StepLimit
    }

    /// Queue a host input event to run inside the next budgeted guest slice.
    pub fn queue_input_event(&mut self, kind: u32, a: i64, b: i64, c: i64) {
        self.pending_input_events.push_back((kind, a, b, c));
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

fn retain_deferred_state_commands(state: &mut hle::com::DeferredContextState, commands: &[u8]) {
    let mut offset = 0usize;
    while offset + 8 <= commands.len() {
        let opcode = u32::from_le_bytes(commands[offset..offset + 4].try_into().unwrap());
        let payload_len =
            u32::from_le_bytes(commands[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let end = offset.saturating_add(8).saturating_add(payload_len);
        if end > commands.len() {
            break;
        }
        let retain = matches!(
            opcode,
            hle::gpu::op::SET_VERTEX_BUFFERS
                | hle::gpu::op::SET_INDEX_BUFFER
                | hle::gpu::op::SET_INPUT_LAYOUT
                | hle::gpu::op::SET_TOPOLOGY
                | hle::gpu::op::SET_SHADER
                | hle::gpu::op::SET_CBUFFERS
                | hle::gpu::op::SET_SRVS
                | hle::gpu::op::SET_SAMPLERS
                | hle::gpu::op::SET_RENDER_TARGETS
                | hle::gpu::op::SET_VIEWPORTS
                | hle::gpu::op::SET_SCISSORS
                | hle::gpu::op::SET_BLEND
                | hle::gpu::op::SET_RASTER
                | hle::gpu::op::SET_DEPTH
                | hle::gpu::op::SET_UAVS
                | hle::gpu::op::SET_CBUFFERS1
                | hle::gpu::op::SET_FIXED_FUNCTION
        );
        if opcode == hle::gpu::op::CLEAR_STATE {
            state.state_commands.clear();
            state
                .state_commands
                .extend_from_slice(&commands[offset..end]);
        } else if retain {
            state
                .state_commands
                .extend_from_slice(&commands[offset..end]);
        }
        offset = end;
    }
}

pub fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod deferred_context_tests {
    use super::*;

    #[test]
    fn dispatch_routes_and_finishes_deferred_context_commands() {
        let mut process = Process::new(Vfs::new());
        let deferred =
            hle::com::create(&mut process, "ID3D11DeviceContext", hle::com::Kind::Context);
        process
            .extra
            .com
            .deferred_contexts
            .insert(deferred, hle::com::DeferredContextState::default());
        let immediate =
            hle::com::create(&mut process, "ID3D11DeviceContext", hle::com::Kind::Context);
        let topology_thunk = process
            .thunk_for("com", "ID3D11DeviceContext::IASetPrimitiveTopology", true)
            .unwrap();
        let finish_thunk = process
            .thunk_for("com", "ID3D11DeviceContext::FinishCommandList", true)
            .unwrap();
        let draw_thunk = process
            .thunk_for("com", "ID3D11DeviceContext::Draw", true)
            .unwrap();
        let stack = 0x10_000;
        let first_list_out = stack + 0x40;
        let second_list_out = stack + 0x48;
        assert!(process.mem.map(stack, 0x1000, PROT_RW));

        process.cpu.gpr[RSP] = stack + 0x800;
        process.wr64(stack + 0x800, 0x1234).unwrap();
        process.cpu.gpr[RCX] = deferred;
        process.cpu.gpr[RDX] = 4;
        process.dispatch_thunk(topology_thunk).unwrap();
        assert!(process.extra.gpu.buf.is_empty());

        process.cpu.gpr[RSP] = stack + 0x800;
        process.wr64(stack + 0x800, 0x1234).unwrap();
        process.cpu.gpr[RCX] = deferred;
        process.cpu.gpr[RDX] = 1; // Preserve deferred state.
        process.cpu.gpr[R8] = first_list_out;
        process.dispatch_thunk(finish_thunk).unwrap();
        let first_list = process.rd64(first_list_out).unwrap();
        assert!(matches!(
            hle::com::kind_of(&process, first_list),
            Some(hle::com::Kind::CommandList { .. })
        ));
        assert_eq!(
            hle::gpu::decode(&process.extra.com.deferred_contexts[&deferred].commands)
                .into_iter()
                .map(|(opcode, _)| opcode)
                .collect::<Vec<_>>(),
            vec![hle::gpu::op::SET_TOPOLOGY]
        );

        process.cpu.gpr[RSP] = stack + 0x800;
        process.wr64(stack + 0x800, 0x1234).unwrap();
        process.cpu.gpr[RCX] = deferred;
        process.cpu.gpr[RDX] = 3;
        process.cpu.gpr[R8] = 0;
        process.dispatch_thunk(draw_thunk).unwrap();
        process.cpu.gpr[RSP] = stack + 0x800;
        process.wr64(stack + 0x800, 0x1234).unwrap();
        process.cpu.gpr[RCX] = deferred;
        process.cpu.gpr[RDX] = 0; // Reset deferred state.
        process.cpu.gpr[R8] = second_list_out;
        process.dispatch_thunk(finish_thunk).unwrap();
        let second_list = process.rd64(second_list_out).unwrap();
        let second_commands = match hle::com::kind_of(&process, second_list).unwrap() {
            hle::com::Kind::CommandList { commands, .. } => hle::gpu::decode(&commands),
            _ => panic!("FinishCommandList must return a command list"),
        };
        assert_eq!(
            second_commands
                .into_iter()
                .map(|(opcode, _)| opcode)
                .collect::<Vec<_>>(),
            vec![hle::gpu::op::SET_TOPOLOGY, hle::gpu::op::DRAW]
        );
        assert!(process.extra.com.deferred_contexts[&deferred]
            .state_commands
            .is_empty());

        process.cpu.gpr[RSP] = stack + 0x800;
        process.wr64(stack + 0x800, 0x1234).unwrap();
        process.cpu.gpr[RCX] = immediate;
        process.cpu.gpr[RDX] = 5;
        process.dispatch_thunk(topology_thunk).unwrap();
        assert_eq!(
            hle::gpu::decode(&process.extra.gpu.buf)
                .into_iter()
                .map(|(opcode, _)| opcode)
                .collect::<Vec<_>>(),
            vec![hle::gpu::op::SET_TOPOLOGY]
        );
        assert!(process.extra.com.deferred_contexts[&deferred]
            .commands
            .is_empty());
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;
    use anywasm_cpu::mem::PAGE_SIZE;

    const CALLBACK_ADDR: u64 = 0x20_000;
    const DLL_ORDER_COUNTER: u64 = 0x30_000;
    const DLL_A_ORDER: u64 = DLL_ORDER_COUNTER + 4;
    const DLL_B_ORDER: u64 = DLL_ORDER_COUNTER + 8;
    const DLL_TLS_CALLBACK_ORDER: u64 = DLL_ORDER_COUNTER + 0x20;
    const DLL_TLS_CALLBACK_ORDER_2: u64 = DLL_ORDER_COUNTER + 0x24;
    const DLL_TLS_CALLBACK_BASE: u64 = DLL_ORDER_COUNTER + 0x28;
    const DLL_TLS_CALLBACK_REASON: u64 = DLL_ORDER_COUNTER + 0x30;
    const DLL_TLS_CALLBACK_RESERVED: u64 = DLL_ORDER_COUNTER + 0x38;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn minimal_pe64(image_base: u64, is_dll: bool, code: &[u8], import: Option<&str>) -> Vec<u8> {
        let mut pe = vec![0u8; 0x600];
        pe[..2].copy_from_slice(b"MZ");
        put_u32(&mut pe, 0x3c, 0x80);
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");

        let coff = 0x84;
        put_u16(&mut pe, coff, anywasm_pe::MACHINE_AMD64);
        put_u16(&mut pe, coff + 2, 1);
        put_u16(&mut pe, coff + 16, 240);
        put_u16(&mut pe, coff + 18, 0x0022 | if is_dll { 0x2000 } else { 0 });

        let optional = coff + 20;
        put_u16(&mut pe, optional, 0x020b);
        put_u32(&mut pe, optional + 16, 0x1000);
        put_u64(&mut pe, optional + 24, image_base);
        put_u32(&mut pe, optional + 32, 0x1000);
        put_u32(&mut pe, optional + 36, 0x200);
        put_u32(&mut pe, optional + 56, 0x2000);
        put_u32(&mut pe, optional + 60, 0x200);
        put_u16(&mut pe, optional + 68, 3);
        put_u64(&mut pe, optional + 72, 0x100000);
        put_u64(&mut pe, optional + 80, 0x1000);
        put_u64(&mut pe, optional + 88, 0x100000);
        put_u64(&mut pe, optional + 96, 0x1000);
        put_u32(&mut pe, optional + 108, 16);
        if import.is_some() {
            put_u32(&mut pe, optional + 112 + 8, 0x1100);
            put_u32(&mut pe, optional + 112 + 12, 40);
        }

        let section = optional + 240;
        pe[section..section + 5].copy_from_slice(b".text");
        put_u32(&mut pe, section + 8, 0x400);
        put_u32(&mut pe, section + 12, 0x1000);
        put_u32(&mut pe, section + 16, 0x400);
        put_u32(&mut pe, section + 20, 0x200);
        put_u32(&mut pe, section + 36, 0x60000020);
        pe[0x200..0x200 + code.len()].copy_from_slice(code);

        if let Some(name) = import {
            // One import descriptor with an empty thunk list is sufficient to load the dependency.
            put_u32(&mut pe, 0x300 + 12, 0x1130);
            put_u32(&mut pe, 0x300 + 16, 0x1140);
            pe[0x330..0x330 + name.len()].copy_from_slice(name.as_bytes());
            pe[0x330 + name.len()] = 0;
        }
        pe
    }

    fn minimal_pe64_with_tls_callbacks(
        image_base: u64,
        is_dll: bool,
        code: &[u8],
        import: Option<&str>,
        callback_codes: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut pe = minimal_pe64(image_base, is_dll, code, import);
        let optional = 0x98;
        let tls_dir = 0x1180usize;
        let tls_dir_file = 0x200 + (tls_dir - 0x1000);
        put_u32(&mut pe, optional + 112 + 9 * 8, tls_dir as u32);
        put_u32(&mut pe, optional + 112 + 9 * 8 + 4, 40);

        // AddressOfIndex and AddressOfCallbacks are VAs in the TLS directory.
        put_u64(&mut pe, tls_dir_file + 16, image_base + 0x11c0);
        put_u64(&mut pe, tls_dir_file + 24, image_base + 0x11d0);
        let callbacks_file = 0x200 + (0x11d0 - 0x1000);
        assert!(callback_codes.len() <= 2);
        for (index, callback_code) in callback_codes.iter().enumerate() {
            assert!(callback_code.len() <= 0x80);
            let callback_rva = 0x1200 + index as u32 * 0x80;
            put_u64(
                &mut pe,
                callbacks_file + index * 8,
                image_base + callback_rva as u64,
            );
            let callback_file = 0x200 + (callback_rva as usize - 0x1000);
            pe[callback_file..callback_file + callback_code.len()].copy_from_slice(callback_code);
        }
        pe
    }

    fn tls_recording_callback_code(order_slot: u64) -> Vec<u8> {
        let mut code = vec![0x48, 0xb8]; // mov rax, counter
        code.extend_from_slice(&DLL_ORDER_COUNTER.to_le_bytes());
        code.extend_from_slice(&[
            0x44, 0x8b, 0x08, // mov r9d, [rax]
            0x41, 0xff, 0xc1, // inc r9d
            0x44, 0x89, 0x08, // mov [rax], r9d
            0x48, 0xb8, // mov rax, callback order slot
        ]);
        code.extend_from_slice(&order_slot.to_le_bytes());
        code.extend_from_slice(&[0x44, 0x89, 0x08, 0x48, 0xb8]); // mov [rax], r9d; mov rax, base slot
        code.extend_from_slice(&DLL_TLS_CALLBACK_BASE.to_le_bytes());
        code.extend_from_slice(&[0x48, 0x89, 0x08, 0x48, 0xb8]); // mov [rax], rcx; mov rax, reason slot
        code.extend_from_slice(&DLL_TLS_CALLBACK_REASON.to_le_bytes());
        code.extend_from_slice(&[0x89, 0x10, 0x48, 0xb8]); // mov [rax], edx; mov rax, reserved slot
        code.extend_from_slice(&DLL_TLS_CALLBACK_RESERVED.to_le_bytes());
        code.extend_from_slice(&[
            0x4c, 0x89, 0x00, // mov [rax], r8
            0xb8, 1, 0, 0, 0, // return TRUE
            0xc3,
        ]);
        code
    }

    fn dll_order_code(result: u64) -> Vec<u8> {
        let mut code = vec![0x48, 0xb8]; // mov rax, counter address
        code.extend_from_slice(&DLL_ORDER_COUNTER.to_le_bytes());
        code.extend_from_slice(&[
            0x8b, 0x08, // mov ecx, [rax]
            0xff, 0xc1, // inc ecx
            0x89, 0x08, // mov [rax], ecx
            0x48, 0xb8, // mov rax, result address
        ]);
        code.extend_from_slice(&result.to_le_bytes());
        code.extend_from_slice(&[
            0x89, 0x08, // mov [rax], ecx
            0xb8, 1, 0, 0, 0, // return TRUE
            0xc3,
        ]);
        code
    }

    fn dll_order_process(load_a_during_startup: bool) -> Process {
        let mut vfs = Vfs::new();
        let exe_import = load_a_during_startup.then_some("a.dll");
        vfs.insert(
            "order.exe",
            minimal_pe64(0x1400_0000_0, false, &[0xc3], exe_import),
        );
        vfs.insert(
            "a.dll",
            minimal_pe64(
                0x1800_0000_0,
                true,
                &dll_order_code(DLL_A_ORDER),
                Some("b.dll"),
            ),
        );
        vfs.insert(
            "b.dll",
            minimal_pe64(0x1900_0000_0, true, &dll_order_code(DLL_B_ORDER), None),
        );
        let mut process = Process::new(vfs);
        assert!(process.mem.map(DLL_ORDER_COUNTER, 0x1000, PROT_RW));
        process.load_exe("order.exe", None).unwrap();
        process
    }

    fn assert_dependency_initialized_first(process: &Process) {
        assert_eq!(process.mem.read_u32(DLL_ORDER_COUNTER).unwrap(), 2);
        assert_eq!(process.mem.read_u32(DLL_B_ORDER).unwrap(), 1);
        assert_eq!(process.mem.read_u32(DLL_A_ORDER).unwrap(), 2);
    }

    #[test]
    fn x64_access_violation_enters_vectored_handler_and_resumes_edited_context() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const FAULT_ADDRESS: u64 = 0xdead_0000;
        const MARKER: u64 = 0x30000;
        const HANDLER_RVA: u64 = 0x1040;
        const LANDING_RVA: u64 = 0x1080;

        let mut code = vec![0x48, 0xb8]; // mov rax, FAULT_ADDRESS
        code.extend_from_slice(&FAULT_ADDRESS.to_le_bytes());
        code.extend_from_slice(&[
            0x48, 0x8b, 0x18, // mov rbx, [rax] -- access violation
            0xb8, 1, 0, 0, 0, // return 1 if the handler resumes at the landing pad
            0xc3,
        ]);
        code.resize(HANDLER_RVA as usize - 0x1000, 0x90);

        let mut handler = vec![
            0x48, 0x8b, 0x01, // mov rax, [rcx] -- EXCEPTION_RECORD
            0x48, 0x8b, 0x50, 0x28, // mov rdx, [rax + 40] -- fault address
            0x48, 0xb8, // mov rax, MARKER
        ];
        handler.extend_from_slice(&MARKER.to_le_bytes());
        handler.extend_from_slice(&[
            0x48, 0x89, 0x10, // mov [rax], rdx
            0x48, 0x8b, 0x41, 0x08, // mov rax, [rcx + 8] -- CONTEXT
            0x48, 0xba, // mov rdx, LANDING
        ]);
        handler.extend_from_slice(&(IMAGE_BASE + LANDING_RVA).to_le_bytes());
        handler.extend_from_slice(&[
            0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
            0xb8, 0xff, 0xff, 0xff, 0xff, // return EXCEPTION_CONTINUE_EXECUTION
            0xc3,
        ]);
        code.extend_from_slice(&handler);
        code.resize(LANDING_RVA as usize - 0x1000, 0x90);
        code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc3]);

        let mut vfs = Vfs::new();
        vfs.insert("seh.exe", minimal_pe64(IMAGE_BASE, false, &code, None));
        let mut process = Process::new(vfs);
        process.load_exe("seh.exe", None).unwrap();
        assert!(process.mem.map(MARKER, 0x1000, PROT_RW));
        process
            .vectored_exception_handlers
            .push((0x7000_0000, IMAGE_BASE + HANDLER_RVA));

        assert_eq!(process.run(200), Stop::Exit(1));
        assert_eq!(process.mem.read_u64(MARKER).unwrap(), FAULT_ADDRESS);
    }

    #[test]
    fn x64_cpu_instruction_exceptions_enter_vectored_handler_and_resume() {
        const IMAGE_BASE: u64 = 0x1400_0000_0;
        const MARKER: u64 = 0x30000;
        const HANDLER_RVA: u64 = 0x1040;
        const LANDING_RVA: u64 = 0x1080;

        for (instruction, expected_code) in [
            // UD2 and an undefined encoding -> STATUS_ILLEGAL_INSTRUCTION.
            (&[0x0f, 0x0b][..], 0xc000_001du32),
            (&[0x0f, 0x0c][..], 0xc000_001d),
            // CLI and STI -> STATUS_PRIVILEGED_INSTRUCTION.
            (&[0xfa][..], 0xc000_0096),
            (&[0xfb][..], 0xc000_0096),
            (&[0x31, 0xc0, 0xf7, 0xf0][..], 0xc000_0094), // DIV EAX by zero
            (&[0xcc][..], 0x8000_0003),                   // INT3
        ] {
            let mut code = instruction.to_vec();
            code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc3]);
            code.resize((HANDLER_RVA - 0x1000) as usize, 0x90);

            let mut handler = vec![
                0x48, 0x8b, 0x01, // mov rax, [rcx] -- EXCEPTION_RECORD
                0x8b, 0x00, // mov eax, [rax] -- ExceptionCode
                0x48, 0xba, // mov rdx, MARKER
            ];
            handler.extend_from_slice(&MARKER.to_le_bytes());
            handler.extend_from_slice(&[
                0x89, 0x02, // mov [rdx], eax
                0x48, 0x8b, 0x41, 0x08, // mov rax, [rcx + 8] -- CONTEXT
                0x48, 0xba, // mov rdx, LANDING
            ]);
            handler.extend_from_slice(&(IMAGE_BASE + LANDING_RVA).to_le_bytes());
            handler.extend_from_slice(&[
                0x48, 0x89, 0x90, 0xf8, 0, 0, 0, // mov [rax + CONTEXT.Rip], rdx
                0xb8, 0xff, 0xff, 0xff, 0xff, // return EXCEPTION_CONTINUE_EXECUTION
                0xc3,
            ]);
            code.extend_from_slice(&handler);
            code.resize((LANDING_RVA - 0x1000) as usize, 0x90);
            code.extend_from_slice(&[0xb8, 7, 0, 0, 0, 0xc3]);

            let mut vfs = Vfs::new();
            vfs.insert(
                "instruction-seh.exe",
                minimal_pe64(IMAGE_BASE, false, &code, None),
            );
            let mut process = Process::new(vfs);
            process.load_exe("instruction-seh.exe", None).unwrap();
            assert!(process.mem.map(MARKER, 0x1000, PROT_RW));
            process
                .vectored_exception_handlers
                .push((0x7000_0000, IMAGE_BASE + HANDLER_RVA));

            assert_eq!(process.run(200), Stop::Exit(7));
            assert_eq!(process.mem.read_u32(MARKER).unwrap(), expected_code);
        }
    }

    #[test]
    fn startup_dll_initialization_runs_dependencies_before_importers() {
        let process = dll_order_process(true);
        assert_dependency_initialized_first(&process);
    }

    #[test]
    fn tls_callbacks_run_before_dllmain_with_process_attach_arguments() {
        let mut vfs = Vfs::new();
        vfs.insert(
            "tls-order.exe",
            minimal_pe64(0x1400_0000_0, false, &[0xc3], Some("a.dll")),
        );
        vfs.insert(
            "a.dll",
            minimal_pe64(
                0x1800_0000_0,
                true,
                &dll_order_code(DLL_A_ORDER),
                Some("b.dll"),
            ),
        );
        vfs.insert(
            "b.dll",
            minimal_pe64_with_tls_callbacks(
                0x1900_0000_0,
                true,
                &dll_order_code(DLL_B_ORDER),
                None,
                &[
                    tls_recording_callback_code(DLL_TLS_CALLBACK_ORDER),
                    tls_recording_callback_code(DLL_TLS_CALLBACK_ORDER_2),
                ],
            ),
        );
        let mut process = Process::new(vfs);
        assert!(process.mem.map(DLL_ORDER_COUNTER, 0x1000, PROT_RW));

        process.load_exe("tls-order.exe", None).unwrap();

        let module = process
            .modules
            .iter()
            .find(|module| module.name == "b.dll")
            .unwrap();
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_ORDER).unwrap(), 1);
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_ORDER_2).unwrap(), 2);
        assert_eq!(
            process.mem.read_u64(DLL_TLS_CALLBACK_BASE).unwrap(),
            module.base
        );
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_REASON).unwrap(), 1);
        assert_eq!(process.mem.read_u64(DLL_TLS_CALLBACK_RESERVED).unwrap(), 0);
        assert_eq!(process.mem.read_u32(DLL_B_ORDER).unwrap(), 3);
        assert_eq!(process.mem.read_u32(DLL_A_ORDER).unwrap(), 4);
    }

    #[test]
    fn executable_tls_callbacks_run_during_process_startup() {
        let mut vfs = Vfs::new();
        vfs.insert(
            "tls-exe.exe",
            minimal_pe64_with_tls_callbacks(
                0x1400_0000_0,
                false,
                &[0xc3],
                None,
                &[tls_recording_callback_code(DLL_TLS_CALLBACK_ORDER)],
            ),
        );
        let mut process = Process::new(vfs);
        assert!(process.mem.map(DLL_ORDER_COUNTER, 0x1000, PROT_RW));

        process.load_exe("tls-exe.exe", None).unwrap();

        let module = &process.modules[0];
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_ORDER).unwrap(), 1);
        assert_eq!(
            process.mem.read_u64(DLL_TLS_CALLBACK_BASE).unwrap(),
            module.base
        );
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_REASON).unwrap(), 1);
        assert_eq!(process.mem.read_u64(DLL_TLS_CALLBACK_RESERVED).unwrap(), 0);
    }

    #[test]
    fn load_library_runs_tls_callbacks_for_a_dll_without_dllmain() {
        let mut vfs = Vfs::new();
        vfs.insert(
            "tls-load.exe",
            minimal_pe64(0x1400_0000_0, false, &[0xc3], None),
        );
        let mut dll = minimal_pe64_with_tls_callbacks(
            0x1800_0000_0,
            true,
            &[0xc3],
            None,
            &[tls_recording_callback_code(DLL_TLS_CALLBACK_ORDER)],
        );
        put_u32(&mut dll, 0x98 + 16, 0); // TLS-only DLL with no DllMain entry point.
        vfs.insert("tls-only.dll", dll);
        let mut process = Process::new(vfs);
        assert!(process.mem.map(DLL_ORDER_COUNTER, 0x1000, PROT_RW));
        process.load_exe("tls-load.exe", None).unwrap();
        let name = process.alloc_virtual(0x1000, PROT_RW);
        process.write_cstr(name, "tls-only.dll").unwrap();
        let load_library = process.thunk_for("kernel32", "LoadLibraryA", true).unwrap();

        let loaded = process.call_guest(load_library, &[name]).unwrap();

        assert_ne!(loaded, 0);
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_ORDER).unwrap(), 1);
        assert_eq!(process.mem.read_u64(DLL_TLS_CALLBACK_BASE).unwrap(), loaded);
        assert_eq!(process.mem.read_u32(DLL_TLS_CALLBACK_REASON).unwrap(), 1);
        assert_eq!(process.mem.read_u64(DLL_TLS_CALLBACK_RESERVED).unwrap(), 0);
    }

    #[test]
    fn load_library_initializes_new_dependencies_before_the_requested_dll() {
        let mut process = dll_order_process(false);
        let name = process.alloc_virtual(0x1000, PROT_RW);
        process.write_cstr(name, "a.dll").unwrap();
        let load_library = process.thunk_for("kernel32", "LoadLibraryA", true).unwrap();

        let loaded = process.call_guest(load_library, &[name]).unwrap();

        assert_ne!(loaded, 0);
        assert_dependency_initialized_first(&process);
    }

    #[test]
    fn x86_stdcall_signatures_remove_argument_bytes_for_spin_count_and_tls_set() {
        assert_eq!(
            stdcall_stack_bytes("kernel32", "InitializeCriticalSectionAndSpinCount"),
            8
        );
        assert_eq!(
            stdcall_stack_bytes("kernel32", "AddVectoredExceptionHandler"),
            8
        );
        assert_eq!(
            stdcall_stack_bytes("kernel32", "RemoveVectoredExceptionHandler"),
            4
        );
        assert_eq!(stdcall_stack_bytes("kernel32", "TlsSetValue"), 8);
        assert_eq!(stdcall_stack_bytes("kernel32", "GetCurrentThreadId"), 0);
        assert_eq!(stdcall_stack_bytes("kernel32", "GetVersionExA"), 4);
    }

    #[test]
    fn x86_import_trace_includes_recent_guest_instructions_when_a_fault_occurs() {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(CALLBACK_ADDR, 0x1000, PROT_RWX));
        process.mem.write_u8(CALLBACK_ADDR, 0x90).unwrap();
        process.mem.write_u8(CALLBACK_ADDR + 1, 0x0f).unwrap();
        process.mem.write_u8(CALLBACK_ADDR + 2, 0x0b).unwrap();
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.rip = CALLBACK_ADDR;
        process.trace_imports = true;

        let Stop::Crash(message) = process.run(10) else {
            panic!("expected an x86 decode fault");
        };
        assert!(message.contains("recent x86 instructions"), "{message}");
        assert!(message.contains("rip=0x00020000"), "{message}");
    }

    fn frame_after_callback(process: &mut Process) -> Result<u64, Stop> {
        process.call_guest(CALLBACK_ADDR, &[])?;
        process.yield_request = Some(Stop::Frame);
        Ok(0)
    }

    fn process_with_teb() -> Process {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        process.mem.write_u64(TEB_ADDR + 0x08, 0x7000).unwrap();
        process.mem.write_u64(TEB_ADDR + 0x10, 0x6000).unwrap();
        process.mem.write_u64(TEB_ADDR + 0x48, 0x1004).unwrap();
        process
    }

    #[test]
    fn host_frame_elapsed_time_100ns_is_converted_to_virtual_10ns_ticks() {
        let mut process = process_with_teb();
        process.retired_instructions = 1234;
        let before = process.now_ticks();

        process.advance_time_100ns(250_000);

        assert_eq!(process.now_ticks(), before + 2_500_000);
        process.retired_instructions += 99;
        assert_eq!(process.now_ticks(), before + 2_500_099);
    }

    #[test]
    fn run_counts_nested_callback_and_yielding_hle_instructions() {
        let mut process = process_with_teb();
        let stack = 0x10_000;
        assert!(process.mem.map(stack, 0x1000, PROT_RW));
        assert!(process.mem.map(CALLBACK_ADDR, 0x1000, PROT_RWX));
        process.mem.write_u8(CALLBACK_ADDR, 0xc3).unwrap(); // ret
        process.cpu.gpr[RSP] = stack + 0x800;
        process.cpu.rip = THUNK_BASE;
        process.wr64(stack + 0x800, 0x30_000).unwrap();
        process.thunks.push(Thunk {
            group: "test",
            name: "frame_after_callback".into(),
            func: Some(frame_after_callback),
        });
        let original_cpu_icount = process.cpu.icount;

        assert_eq!(process.run(2), Stop::Frame);
        assert_eq!(process.retired_instructions, 2);
        assert_eq!(process.cpu.icount, original_cpu_icount);
    }

    #[test]
    fn synchronous_callback_cannot_run_past_the_active_budget() {
        let mut process = process_with_teb();
        let stack = 0x10_000;
        assert!(process.mem.map(stack, 0x1000, PROT_RW));
        assert!(process.mem.map(CALLBACK_ADDR, 0x1000, PROT_RWX));
        process
            .mem
            .write_force(CALLBACK_ADDR, &[0xeb, 0xfe])
            .unwrap(); // jmp to self
        process.cpu.gpr[RSP] = stack + 0x800;
        process.cpu.rip = THUNK_BASE;
        process.wr64(stack + 0x800, 0x30_000).unwrap();
        process.thunks.push(Thunk {
            group: "test",
            name: "nonterminating_callback".into(),
            func: Some(frame_after_callback),
        });

        let stop = process.run(2);

        assert!(matches!(
            stop,
            Stop::Crash(message) if message.contains("callback exhausted the run budget")
        ));
        assert_eq!(process.retired_instructions, 2);
        assert_eq!(process.nested, 0);
        assert!(process.active_run_budget.is_none());
    }

    #[test]
    fn nonterminating_atexit_callback_reports_budget_exhaustion() {
        let mut process = process_with_teb();
        let stack = 0x10_000;
        assert!(process.mem.map(stack, 0x1000, PROT_RW));
        assert!(process.mem.map(CALLBACK_ADDR, 0x1000, PROT_RWX));
        process
            .mem
            .write_force(CALLBACK_ADDR, &[0xeb, 0xfe])
            .unwrap(); // jmp to self
        process.cpu.gpr[RSP] = stack + 0x800;
        process.cpu.gpr[RAX] = 0;
        process.cpu.rip = EXIT_MAGIC;
        process.atexit.push(CALLBACK_ADDR);

        let stop = process.run(3);

        assert!(matches!(
            stop,
            Stop::Crash(message) if message.contains("callback exhausted the run budget")
        ));
        assert_eq!(process.retired_instructions, 3);
        assert_eq!(process.exit_code, None);
        assert!(process.atexit.is_empty());
    }

    #[test]
    fn faulting_atexit_callback_propagates_its_crash() {
        let mut process = process_with_teb();
        let stack = 0x10_000;
        assert!(process.mem.map(stack, 0x1000, PROT_RW));
        assert!(process.mem.map(CALLBACK_ADDR, 0x1000, PROT_RWX));
        process.mem.write_u8(CALLBACK_ADDR, 0xcc).unwrap(); // int3
        process.cpu.gpr[RSP] = stack + 0x800;
        process.cpu.gpr[RAX] = 0;
        process.cpu.rip = EXIT_MAGIC;
        process.atexit.push(CALLBACK_ADDR);

        let stop = process.run(10);

        assert!(
            matches!(stop, Stop::Crash(ref message) if message.contains("Breakpoint")),
            "unexpected exit result: {stop:?}"
        );
        assert_eq!(process.exit_code, None);
        assert!(process.atexit.is_empty());
    }

    #[test]
    fn run_preserves_retirement_count_before_a_later_block_fault() {
        let mut process = process_with_teb();
        let code = 0x20_000;
        assert!(process.mem.map(code, 0x1000, PROT_RWX));
        process.mem.write_force(code, &[0x90, 0xcc]).unwrap(); // nop; int3
        process.cpu.rip = code;

        assert!(matches!(process.run(10), Stop::Crash(_)));
        assert_eq!(process.retired_instructions, 1);
    }

    #[test]
    fn trace_imports_include_arguments_and_caller_for_tls_get_value() {
        let mut process = process_with_teb();
        let stack_base = 0x10000;
        let stack = stack_base + 0x800;
        assert!(process.mem.map(stack_base, 0x1000, PROT_RW));
        process.trace_imports = true;
        process.cpu.gpr[RCX] = 7;
        process.cpu.gpr[RSP] = stack;
        process.wr64(stack, 0x140001234).unwrap();
        process.tls_slots.insert(7, 0x777);
        process.modules.push(Module {
            name: "fixture.dll".into(),
            path: "C:\\fixture.dll".into(),
            base: 0x140000000,
            size: 0x10000,
            entry: 0,
            is_dll: true,
            tls_callbacks: Vec::new(),
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: 0,
            resource_size: 0,
            hle_group: None,
        });
        let thunk = process.thunk_for("kernel32", "TlsGetValue", false).unwrap();

        process.dispatch_thunk(thunk).unwrap();

        let trace = String::from_utf8(process.stderr).unwrap();
        assert!(trace.contains(
            "[import] kernel32!TlsGetValue args=[0x7] caller=0x140001234 module=fixture.dll+0x1234"
        ));
        assert_eq!(process.cpu.gpr[RAX], 0x777);
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
    fn pe32_exception_registration_head_is_saved_per_thread() {
        let mut process = Process::new(Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.fs_base = TEB_ADDR;
        process.vmem_next = 0x2000_0000;
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(0x60000, 0x10000, PROT_RW));
        process.wr32(TEB_ADDR, 0x61000).unwrap();
        process.wr32(TEB_ADDR + 4, 0x70000).unwrap();
        process.wr32(TEB_ADDR + 8, 0x60000).unwrap();
        process.wr32(TEB_ADDR + 0x2c, 0).unwrap();
        process.wr32(TEB_ADDR + 0x34, 0).unwrap();

        process
            .create_guest_thread(0x1008, 0x2000, 0, false)
            .unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.rd32(TEB_ADDR).unwrap(), u32::MAX);

        process.wr32(TEB_ADDR, 0x6f000).unwrap();
        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1004);
        assert_eq!(process.rd32(TEB_ADDR).unwrap(), 0x61000);

        process.request_thread_yield();
        assert!(process.schedule_thread().unwrap());
        assert_eq!(process.current_thread_id, 0x1008);
        assert_eq!(process.rd32(TEB_ADDR).unwrap(), 0x6f000);
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
