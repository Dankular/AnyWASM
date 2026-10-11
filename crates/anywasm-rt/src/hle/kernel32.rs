//! kernel32 / kernelbase / api-ms-win-core-* host implementation.

use super::*;
use crate::process::*;
use crate::vfs::{absolute, norm, CreateDirError};
use anywasm_cpu::cpu::RSP;
use anywasm_cpu::mem::{PROT_NONE, PROT_R, PROT_RW, PROT_RWX, PROT_X};

const INVALID_HANDLE: u64 = u64::MAX;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_TIMEOUT: u32 = 1460;
const ERROR_MOD_NOT_FOUND: u32 = 126;
const ERROR_PROC_NOT_FOUND: u32 = 127;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_NOT_OWNER: u32 = 288;
const ERROR_OLD_WIN_VERSION: u32 = 1150;
const WAIT_OBJECT_0: u64 = 0;
const WAIT_TIMEOUT: u64 = 0x102;
const WAIT_FAILED: u64 = u32::MAX as u64;

pub const STD_IN: u64 = 0x10;
pub const STD_OUT: u64 = 0x14;
pub const STD_ERR: u64 = 0x18;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        // process / thread
        "ExitProcess" => exit_process,
        "TerminateProcess" => terminate_process,
        "ExitThread" => exit_thread,
        "GetCurrentProcess" => |_| Ok(u64::MAX),
        "GetCurrentThread" => |_| Ok(u64::MAX - 1),
        "GetCurrentProcessId" => |_| Ok(0x1000),
        "GetCurrentThreadId" => |p| Ok(p.current_thread_id as u64),
        "GetThreadId" => |p| {
            let h = p.arg(0);
            Ok(match p.handles.get(&h) {
                Some(Handle::Thread { id, .. }) => *id as u64,
                _ if h == u64::MAX - 1 => p.current_thread_id as u64,
                _ => 0,
            })
        },
        "GetExitCodeThread" => |p| {
            let (h, out) = (p.arg(0), p.arg(1));
            let code = match p.handles.get(&h) {
                Some(Handle::Thread {
                    exit_code: Some(code),
                    ..
                }) => *code,
                Some(Handle::Thread { .. }) => 259, // STILL_ACTIVE
                _ => return Ok(0),
            };
            p.wr32(out, code)?;
            Ok(1)
        },
        "GetProcessId" => |_| Ok(0x1000),
        "InitializeSListHead" => |p| {
            // Win64 SLIST_HEADER is a 16-byte, 16-byte-aligned opaque structure.
            p.write_bytes(p.arg(0), &[0; 16])?;
            Ok(0)
        },
        "GetLastError" => |p| Ok(p.rd32(TEB_ADDR + 0x68)? as u64),
        "SetLastError" => |p| {
            let v = p.arg(0) as u32;
            p.set_last_error(v);
            Ok(0)
        },
        "IsDebuggerPresent" => |_| Ok(0),
        "CheckRemoteDebuggerPresent" => |p| {
            let a = p.arg(1);
            if a != 0 {
                p.wr32(a, 0)?;
            }
            Ok(1)
        },
        "DebugBreak" => |_| Err(Stop::Crash("DebugBreak".into())),
        "OutputDebugStringA" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            p.stderr.extend_from_slice(s.as_bytes());
            Ok(0)
        },
        "OutputDebugStringW" => |p| {
            let s = p.read_wstr(p.arg(0))?;
            p.stderr.extend_from_slice(s.as_bytes());
            Ok(0)
        },
        "SetUnhandledExceptionFilter" => |_| Ok(0),
        "UnhandledExceptionFilter" => |_| Ok(0),
        "SetErrorMode" | "SetThreadErrorMode" => |_| Ok(0),
        "RaiseException" => raise_exception,
        "RtlCaptureContext" => ntdll::rtl_capture_context,
        "IsProcessorFeaturePresent" => is_processor_feature_present,
        "GetSystemInfo" | "GetNativeSystemInfo" => get_system_info,
        "GlobalMemoryStatusEx" => global_memory_status_ex,
        "GetVersion" => |_| Ok(0x4A61_0A00),
        "GetVersionExA" | "GetVersionExW" => get_version_ex,
        "VerifyVersionInfoA" | "VerifyVersionInfoW" => verify_version_info,
        "VerSetConditionMask" => |p| {
            let (mut mask, type_mask, condition) =
                (p.arg(0), p.arg(1) as u32, (p.arg(2) as u8 as u64) & 7);
            if type_mask != 0 {
                let bit = 31 - type_mask.leading_zeros();
                let shift = bit * 3;
                if shift < 63 {
                    mask = (mask & !(7u64 << shift)) | (condition << shift);
                }
            }
            Ok(mask | (1u64 << 63))
        },
        "GetComputerNameA" => |p| {
            let (b, n) = (p.arg(0), p.arg(1));
            p.write_cstr(b, "ANYWASM")?;
            p.wr32(n, 7)?;
            Ok(1)
        },
        // memory
        "GetProcessHeap" => |p| Ok(p.heap.process_heap),
        "HeapCreate" => |p| Ok(p.heap.process_heap + 0x1000 * (1 + p.next_handle % 7)),
        "HeapDestroy" => |_| Ok(1),
        "HeapAlloc" => heap_alloc,
        "HeapReAlloc" => heap_realloc,
        "HeapFree" => heap_free,
        "HeapSize" => |p| Ok(p.heap.size_of(p.arg(2)).unwrap_or(u64::MAX)),
        "HeapValidate" => |_| Ok(1),
        "HeapSetInformation" | "HeapLock" | "HeapUnlock" | "HeapCompact" => |_| Ok(1),
        "HeapWalk" => |_| Ok(0),
        "LocalAlloc" | "GlobalAlloc" => local_alloc,
        "LocalFree" | "GlobalFree" => |p| {
            let a = p.arg(0);
            free_block(p, a);
            Ok(0)
        },
        "LocalReAlloc" | "GlobalReAlloc" => |p| {
            let (a, n) = (p.arg(0), p.arg(1));
            realloc_block(p, a, n, 0x40)
        },
        "LocalLock" | "GlobalLock" => |p| Ok(p.arg(0)),
        "LocalUnlock" | "GlobalUnlock" => |_| Ok(1),
        "VirtualAlloc" => virtual_alloc,
        "VirtualFree" => virtual_free,
        "VirtualProtect" => virtual_protect,
        "VirtualQuery" => virtual_query,
        "VirtualLock" | "VirtualUnlock" => |_| Ok(1),
        "FlushInstructionCache" => |_| Ok(1),
        // console / files
        "GetStdHandle" => |p| {
            Ok(match p.arg(0) as u32 {
                0xfffffff6 => STD_IN,
                0xfffffff5 => STD_OUT,
                0xfffffff4 => STD_ERR,
                _ => INVALID_HANDLE,
            })
        },
        "SetStdHandle" => |_| Ok(1),
        "WriteFile" => write_file,
        "ReadFile" => read_file,
        "WriteConsoleA" => write_console_a,
        "WriteConsoleW" => write_console_w,
        "ReadConsoleA" | "ReadConsoleW" => |p| {
            let w = p.arg(3);
            if w != 0 {
                p.wr32(w, 0)?;
            }
            Ok(1)
        },
        "GetConsoleMode" => |p| {
            let m = p.arg(1);
            p.wr32(m, 3)?;
            Ok(1)
        },
        "SetConsoleMode"
        | "SetConsoleCtrlHandler"
        | "SetConsoleTitleA"
        | "SetConsoleTitleW"
        | "FlushConsoleInputBuffer" => |_| Ok(1),
        "GetConsoleCP" | "GetConsoleOutputCP" => |_| Ok(437),
        "SetConsoleOutputCP" | "SetConsoleCP" => |_| Ok(1),
        "GetConsoleScreenBufferInfo" => get_console_screen_buffer_info,
        "AllocConsole" | "FreeConsole" | "AttachConsole" => |_| Ok(1),
        "GetConsoleWindow" => |_| Ok(0),
        "GetFileType" => |p| {
            Ok(match p.arg(0) {
                STD_IN | STD_OUT | STD_ERR => 2,
                h if p.handles.contains_key(&h) => 1,
                _ => 0,
            })
        },
        "CreateFileA" => |p| create_file(p, false),
        "CreateFileW" => |p| create_file(p, true),
        "CloseHandle" => |p| {
            let h = p.arg(0);
            Ok(p.handles.remove(&h).is_some() as u64
                | (matches!(h, STD_IN | STD_OUT | STD_ERR)) as u64)
        },
        "GetFileSize" => |p| {
            let h = p.arg(0);
            match file_len(p, h) {
                Some(n) => {
                    let hi = p.arg(1);
                    if hi != 0 {
                        p.wr32(hi, (n >> 32) as u32)?;
                    }
                    Ok(n & 0xffff_ffff)
                }
                None => Ok(0xffff_ffff),
            }
        },
        "GetFileSizeEx" => |p| match file_len(p, p.arg(0)) {
            Some(n) => {
                let o = p.arg(1);
                p.wr64(o, n)?;
                Ok(1)
            }
            None => Ok(0),
        },
        "SetFilePointer" => set_file_pointer,
        "SetFilePointerEx" => set_file_pointer_ex,
        "SetEndOfFile" | "FlushFileBuffers" => |_| Ok(1),
        "DeleteFileA" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            Ok(p.vfs.remove(&absolute(&s, &p.current_directory)) as u64)
        },
        "DeleteFileW" => |p| {
            let s = p.read_wstr(p.arg(0))?;
            Ok(p.vfs.remove(&absolute(&s, &p.current_directory)) as u64)
        },
        "GetFileAttributesA" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            Ok(file_attrs(p, &s))
        },
        "GetFileAttributesW" => |p| {
            let s = p.read_wstr(p.arg(0))?;
            Ok(file_attrs(p, &s))
        },
        "CreateDirectoryA" => |p| {
            let path = p.read_cstr(p.arg(0))?;
            create_directory(p, &path)
        },
        "CreateDirectoryW" => |p| {
            let path = p.read_wstr(p.arg(0))?;
            create_directory(p, &path)
        },
        "GetCurrentDirectoryA" => |p| get_current_directory(p, false),
        "GetCurrentDirectoryW" => |p| get_current_directory(p, true),
        "SetCurrentDirectoryA" => |p| {
            let path = p.read_cstr(p.arg(0))?;
            set_current_directory(p, &path)
        },
        "SetCurrentDirectoryW" => |p| {
            let path = p.read_wstr(p.arg(0))?;
            set_current_directory(p, &path)
        },
        "GetTempPathA" => |p| {
            let (n, b) = (p.arg(0), p.arg(1));
            if n >= 10 {
                p.write_cstr(b, "C:\\Temp\\")?;
            }
            Ok(8)
        },
        "GetTempPathW" => |p| {
            let (n, b) = (p.arg(0), p.arg(1));
            if n >= 10 {
                p.write_wstr(b, "C:\\Temp\\")?;
            }
            Ok(8)
        },
        "GetWindowsDirectoryA" | "GetSystemDirectoryA" => |p| {
            let (b, n) = (p.arg(0), p.arg(1));
            let s = "C:\\Windows\\System32";
            if n as usize > s.len() {
                p.write_cstr(b, s)?;
            }
            Ok(s.len() as u64)
        },
        "GetWindowsDirectoryW" | "GetSystemDirectoryW" => |p| {
            let (b, n) = (p.arg(0), p.arg(1));
            let s = "C:\\Windows\\System32";
            if n as usize > s.len() {
                p.write_wstr(b, s)?;
            }
            Ok(s.len() as u64)
        },
        "GetFullPathNameA" => |p| full_path(p, false),
        "GetFullPathNameW" => |p| full_path(p, true),
        "GetModuleFileNameA" => |p| module_file_name(p, false),
        "GetModuleFileNameW" => |p| module_file_name(p, true),
        "GetCommandLineA" => get_command_line_a,
        "GetCommandLineW" => get_command_line_w,
        "GetStartupInfoA" | "GetStartupInfoW" => |p| {
            let a = p.arg(0);
            p.write_bytes(a, &[0u8; 104])?;
            p.wr32(a, 104)?;
            Ok(0)
        },
        "GetEnvironmentStringsW" => get_environment_strings_w,
        "GetEnvironmentStrings" | "GetEnvironmentStringsA" => get_environment_strings_a,
        "FreeEnvironmentStringsW" | "FreeEnvironmentStringsA" => |_| Ok(1),
        "GetEnvironmentVariableA" => |p| env_var(p, false),
        "GetEnvironmentVariableW" => |p| env_var(p, true),
        "SetEnvironmentVariableA" | "SetEnvironmentVariableW" => |_| Ok(1),
        "ExpandEnvironmentStringsA" => |p| {
            let (s, d, n) = (p.arg(0), p.arg(1), p.arg(2));
            let str_ = p.read_cstr(s)?;
            if (str_.len() as u64) < n {
                p.write_cstr(d, &str_)?;
            }
            Ok(str_.len() as u64 + 1)
        },
        // modules
        "GetModuleHandleA" => |p| {
            let a = p.arg(0);
            let n = if a == 0 { None } else { Some(p.read_cstr(a)?) };
            module_handle(p, n)
        },
        "GetModuleHandleW" => |p| {
            let a = p.arg(0);
            let n = if a == 0 { None } else { Some(p.read_wstr(a)?) };
            module_handle(p, n)
        },
        "GetModuleHandleExW" => |p| {
            let (flags, a, out) = (p.arg(0), p.arg(1), p.arg(2));
            let n = if a == 0 {
                None
            } else if flags & 4 != 0 {
                None
            } else {
                Some(p.read_wstr(a)?)
            };
            let h = if flags & 4 != 0 {
                p.modules
                    .iter()
                    .find(|m| a >= m.base && a < m.base + m.size)
                    .map(|m| m.base)
                    .unwrap_or(0)
            } else {
                module_handle(p, n)?
            };
            if out != 0 {
                p.wr64(out, h)?;
            }
            Ok((h != 0) as u64)
        },
        "LoadLibraryA" => |p| {
            let n = p.read_cstr(p.arg(0))?;
            load_library(p, &n)
        },
        "LoadLibraryW" => |p| {
            let n = p.read_wstr(p.arg(0))?;
            load_library(p, &n)
        },
        "LoadLibraryExA" => |p| {
            let n = p.read_cstr(p.arg(0))?;
            load_library(p, &n)
        },
        "LoadLibraryExW" => |p| {
            let n = p.read_wstr(p.arg(0))?;
            load_library(p, &n)
        },
        "FreeLibrary" => |_| Ok(1),
        "DisableThreadLibraryCalls" => |_| Ok(1),
        "GetProcAddress" => get_proc_address,
        // time
        "QueryPerformanceCounter" => |p| {
            let a = p.arg(0);
            let t = ticks(p) / 10;
            p.wr64(a, t)?;
            Ok(1)
        },
        "QueryPerformanceFrequency" => |p| {
            let a = p.arg(0);
            p.wr64(a, 10_000_000)?;
            Ok(1)
        },
        "GetTickCount" => |p| Ok((ticks(p) / 100_000) & 0xffff_ffff),
        "GetTickCount64" => |p| Ok(ticks(p) / 100_000),
        "GetSystemTimeAsFileTime" | "GetSystemTimePreciseAsFileTime" => |p| {
            let a = p.arg(0);
            let ft = file_time(p);
            p.wr64(a, ft)?;
            Ok(0)
        },
        "GetSystemTime" | "GetLocalTime" => get_system_time,
        "Sleep" => |p| sleep_thread(p, false),
        "SleepEx" => |p| sleep_thread(p, p.arg(1) != 0),
        "SwitchToThread" => |p| {
            p.request_thread_yield();
            Ok(1)
        },
        "GetTimeZoneInformation" => |p| {
            let a = p.arg(0);
            p.write_bytes(a, &[0u8; 172])?;
            Ok(0)
        },
        "GetSystemTimeAdjustment" => |_| Ok(0),
        // sync
        "InitializeCriticalSection"
        | "InitializeCriticalSectionEx"
        | "InitializeCriticalSectionAndSpinCount" => |p| {
            p.critical_sections.entry(p.arg(0)).or_default();
            Ok(1)
        },
        "EnterCriticalSection" => enter_critical_section,
        "LeaveCriticalSection" => |p| {
            let a = p.arg(0);
            let _ = p.release_critical(a, p.current_thread_id);
            Ok(0)
        },
        "DeleteCriticalSection" => |p| {
            p.critical_sections.remove(&p.arg(0));
            Ok(0)
        },
        "TryEnterCriticalSection" => |p| {
            let a = p.arg(0);
            Ok(p.try_acquire_critical(a, p.current_thread_id) as u64)
        },
        "InitializeSRWLock" => |p| {
            p.srw_locks.entry(p.arg(0)).or_default();
            Ok(0)
        },
        "AcquireSRWLockExclusive" => |p| acquire_srw(p, true),
        "AcquireSRWLockShared" => |p| acquire_srw(p, false),
        "ReleaseSRWLockExclusive" => |p| {
            let a = p.arg(0);
            let _ = p.release_srw(a, p.current_thread_id, true);
            Ok(0)
        },
        "ReleaseSRWLockShared" => |p| {
            let a = p.arg(0);
            let _ = p.release_srw(a, p.current_thread_id, false);
            Ok(0)
        },
        "TryAcquireSRWLockExclusive" => |p| {
            let a = p.arg(0);
            Ok(p.try_acquire_srw(a, p.current_thread_id, true) as u64)
        },
        "TryAcquireSRWLockShared" => |p| {
            let a = p.arg(0);
            Ok(p.try_acquire_srw(a, p.current_thread_id, false) as u64)
        },
        "InitializeConditionVariable" => |_| Ok(0),
        "WakeConditionVariable" => |p| {
            p.wake_condition(p.arg(0), false);
            Ok(0)
        },
        "WakeAllConditionVariable" => |p| {
            p.wake_condition(p.arg(0), true);
            Ok(0)
        },
        "SleepConditionVariableCS" => sleep_condition_variable_cs,
        "SleepConditionVariableSRW" => sleep_condition_variable_srw,
        "InitOnceExecuteOnce" => init_once_execute_once,
        "InitOnceBeginInitialize" => |p| {
            let (once, _fl, pending, _ctx) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
            let done = p.rd64(once)? != 0;
            if pending != 0 {
                p.wr32(pending, (!done) as u32)?;
            }
            Ok(1)
        },
        "InitOnceComplete" => |p| {
            let once = p.arg(0);
            p.wr64(once, 1)?;
            Ok(1)
        },
        "CreateEventA" | "CreateEventW" => |p| {
            let (manual, init) = (p.arg(1) != 0, p.arg(2) != 0);
            Ok(p.new_handle(Handle::Event {
                signaled: init,
                manual,
            }))
        },
        "CreateEventExW" => |p| {
            let flags = p.arg(2) as u32;
            Ok(p.new_handle(Handle::Event {
                signaled: flags & 2 != 0,
                manual: flags & 1 != 0,
            }))
        },
        "SetEvent" => |p| {
            let h = p.arg(0);
            if let Some(Handle::Event { signaled, .. }) = p.handles.get_mut(&h) {
                *signaled = true;
                Ok(1)
            } else {
                p.set_last_error(ERROR_INVALID_HANDLE);
                Ok(0)
            }
        },
        "ResetEvent" => |p| {
            let h = p.arg(0);
            if let Some(Handle::Event { signaled, .. }) = p.handles.get_mut(&h) {
                *signaled = false;
                Ok(1)
            } else {
                p.set_last_error(ERROR_INVALID_HANDLE);
                Ok(0)
            }
        },
        "CreateMutexA" | "CreateMutexW" => create_mutex,
        "CreateMutexExW" => create_mutex_ex,
        "ReleaseMutex" => release_mutex,
        "CreateSemaphoreA" | "CreateSemaphoreW" => create_semaphore,
        "CreateSemaphoreExW" => create_semaphore_ex,
        "ReleaseSemaphore" => release_semaphore,
        "WaitForSingleObject" => |p| wait_for_single_object(p, false),
        "WaitForSingleObjectEx" => |p| wait_for_single_object(p, p.arg(2) != 0),
        "WaitForMultipleObjects" => |p| wait_for_multiple_objects(p, false),
        "WaitForMultipleObjectsEx" => |p| wait_for_multiple_objects(p, p.arg(4) != 0),
        "InterlockedIncrement" => |p| interlocked(p, 4, |v, _| v.wrapping_add(1), true),
        "InterlockedDecrement" => |p| interlocked(p, 4, |v, _| v.wrapping_sub(1), true),
        "InterlockedIncrement64" => |p| interlocked(p, 8, |v, _| v.wrapping_add(1), true),
        "InterlockedDecrement64" => |p| interlocked(p, 8, |v, _| v.wrapping_sub(1), true),
        "InterlockedExchange" => |p| interlocked(p, 4, |_, a| a, false),
        "InterlockedExchange64" => |p| interlocked(p, 8, |_, a| a, false),
        "InterlockedExchangeAdd" => |p| interlocked(p, 4, |v, a| v.wrapping_add(a), false),
        "InterlockedExchangeAdd64" => |p| interlocked(p, 8, |v, a| v.wrapping_add(a), false),
        "InterlockedCompareExchange" => |p| cmpxchg(p, 4),
        "InterlockedCompareExchange64" => |p| cmpxchg(p, 8),
        "InterlockedCompareExchangePointer" => |p| cmpxchg(p, 8),
        "InterlockedExchangePointer" => |p| interlocked(p, 8, |_, a| a, false),
        // TLS / FLS
        "TlsAlloc" => |p| {
            let i = p.tls_next;
            p.tls_next += 1;
            p.tls_slots.insert(i, 0);
            Ok(i as u64)
        },
        "TlsFree" => |p| {
            let i = p.arg(0) as u32;
            Ok(p.tls_slots.remove(&i).is_some() as u64)
        },
        "TlsGetValue" => |p| {
            let i = p.arg(0) as u32;
            p.set_last_error(0);
            Ok(p.tls_slots.get(&i).copied().unwrap_or(0))
        },
        "TlsSetValue" => |p| {
            let (i, v) = (p.arg(0) as u32, p.arg(1));
            p.tls_slots.insert(i, v);
            Ok(1)
        },
        "FlsAlloc" => |p| {
            let i = p.fls_next;
            p.fls_next += 1;
            p.fls.insert(i, 0);
            Ok(i as u64)
        },
        "FlsFree" => |p| {
            let i = p.arg(0) as u32;
            Ok(p.fls.remove(&i).is_some() as u64)
        },
        "FlsGetValue" => |p| {
            let i = p.arg(0) as u32;
            Ok(p.fls.get(&i).copied().unwrap_or(0))
        },
        "FlsSetValue" => |p| {
            let (i, v) = (p.arg(0) as u32, p.arg(1));
            p.fls.insert(i, v);
            Ok(1)
        },
        // strings / locale
        "GetACP" => |_| Ok(1252),
        "GetOEMCP" => |_| Ok(437),
        "IsValidCodePage" => |_| Ok(1),
        "GetCPInfo" => |p| {
            let a = p.arg(1);
            p.write_bytes(a, &[0u8; 20])?;
            p.wr32(a, 1)?;
            Ok(1)
        },
        "GetUserDefaultLCID" | "GetSystemDefaultLCID" | "GetThreadLocale" => |_| Ok(0x409),
        "GetUserDefaultLocaleName" | "GetSystemDefaultLocaleName" => |p| {
            let (b, n) = (p.arg(0), p.arg(1));
            if n >= 6 {
                p.write_wstr(b, "en-US")?;
            }
            Ok(6)
        },
        "GetLocaleInfoA" | "GetLocaleInfoW" | "GetLocaleInfoEx" => |p| {
            let (buf, n) = (p.arg(2), p.arg(3));
            if buf != 0 && n >= 2 {
                p.wr16(buf, 0)?;
            }
            Ok(1)
        },
        "MultiByteToWideChar" => multi_byte_to_wide_char,
        "WideCharToMultiByte" => wide_char_to_multi_byte,
        "lstrlenA" => |p| Ok(p.read_cstr(p.arg(0))?.len() as u64),
        "lstrlenW" => |p| Ok(p.read_wstr(p.arg(0))?.encode_utf16().count() as u64),
        "lstrcpyA" => |p| {
            let (d, s) = (p.arg(0), p.read_cstr(p.arg(1))?);
            p.write_cstr(d, &s)?;
            Ok(d)
        },
        "lstrcmpA" | "lstrcmpiA" => |p| {
            let (a, b) = (p.read_cstr(p.arg(0))?, p.read_cstr(p.arg(1))?);
            Ok(a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()) as i32 as i64 as u64)
        },
        "CompareStringW" | "CompareStringEx" | "CompareStringA" => |p| {
            // (locale, flags, s1, n1, s2, n2) -> CSTR_LESS_THAN 1 / EQUAL 2 / GREATER 3
            let wide = true;
            let _ = wide;
            let (s1, n1, s2, n2) = (p.arg(2), p.arg(3) as i64, p.arg(4), p.arg(5) as i64);
            let a = read_wide_n(p, s1, n1)?;
            let b = read_wide_n(p, s2, n2)?;
            Ok(match a.cmp(&b) {
                std::cmp::Ordering::Less => 1,
                std::cmp::Ordering::Equal => 2,
                std::cmp::Ordering::Greater => 3,
            })
        },
        "GetStringTypeW" | "GetStringTypeExW" | "GetStringTypeA" => get_string_type_w,
        "LCMapStringW" | "LCMapStringEx" => lc_map_string_w,
        "FoldStringW" => |_| Ok(0),
        "IsDBCSLeadByte" | "IsDBCSLeadByteEx" => |_| Ok(0),
        "FormatMessageA" | "FormatMessageW" => |_| Ok(0),
        // misc
        "Beep" => |_| Ok(1),
        "CreateThread" => create_thread,
        "ResumeThread" => resume_thread,
        "SuspendThread" => |p| {
            if matches!(p.handles.get(&p.arg(0)), Some(Handle::Thread { .. })) {
                Ok(0)
            } else {
                Ok(u32::MAX as u64)
            }
        },
        "GetThreadContext" | "SetThreadContext" => |_| Ok(0),
        "SetThreadPriority"
        | "SetPriorityClass"
        | "SetThreadAffinityMask"
        | "SetProcessAffinityMask"
        | "SetThreadDescription" => |_| Ok(1),
        "GetThreadPriority" => |_| Ok(0),
        "GetProcessAffinityMask" => |p| {
            let (a, b) = (p.arg(1), p.arg(2));
            p.wr64(a, 1)?;
            p.wr64(b, 1)?;
            Ok(1)
        },
        "GetLogicalProcessorInformation" => |p| {
            p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
            let n = p.arg(1);
            if n != 0 {
                p.wr32(n, 0)?;
            }
            Ok(0)
        },
        "GetProcessTimes" | "GetThreadTimes" => |p| {
            let a = p.arg(1);
            p.write_bytes(a, &[0u8; 32])?;
            Ok(1)
        },
        "GetProcessWorkingSetSize" | "SetProcessWorkingSetSize" | "SetProcessWorkingSetSizeEx" => {
            |_| Ok(1)
        }
        "AreFileApisANSI" => |_| Ok(1),
        "SetHandleCount" => |p| Ok(p.arg(0)),
        "SetHandleInformation" | "GetHandleInformation" => |_| Ok(1),
        "DuplicateHandle" => |p| {
            let (src, out) = (p.arg(1), p.arg(3));
            p.wr64(out, src)?;
            Ok(1)
        },
        "EncodePointer" | "DecodePointer" | "EncodeSystemPointer" | "DecodeSystemPointer" => {
            |p| Ok(p.arg(0))
        }
        "RtlLookupFunctionEntry" => |_| Ok(0),
        "IsWow64Process" => |p| {
            let a = p.arg(1);
            p.wr32(a, 0)?;
            Ok(1)
        },
        "GetSystemFirmwareTable" => |_| Ok(0),
        "RtlAddFunctionTable" | "RtlDeleteFunctionTable" => |_| Ok(1),
        "AddVectoredExceptionHandler" => |_| Ok(0x7000),
        "RemoveVectoredExceptionHandler" => |_| Ok(1),
        "GetUserDefaultUILanguage" => |_| Ok(0x409),
        "SetThreadStackGuarantee" => |_| Ok(1),
        _ => return None,
    })
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn ticks(p: &Process) -> u64 {
    p.now_ticks()
}

fn file_time(p: &Process) -> u64 {
    // 2026-01-01T00:00:00Z in FILETIME (100 ns since 1601) plus virtual elapsed time.
    const BASE: u64 = 134_100_000_000_000_000 + 0;
    BASE + ticks(p) / 100_000 * 10_000
}

fn exit_process(p: &mut Process) -> R {
    let code = p.arg(0) as u32;
    if p.trace_imports {
        let caller = p
            .mem
            .read_u64(p.cpu.gpr[anywasm_cpu::cpu::RSP])
            .unwrap_or(0);
        p.stderr.extend_from_slice(
            format!("[process] ExitProcess code={code} caller={caller:#x}\n").as_bytes(),
        );
    }
    p.finish_exit(code).map(|_| 0)
}

fn raise_exception(p: &mut Process) -> R {
    let (code, flags, arg_count, args_pointer) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        (p.arg(2) as u32).min(15),
        p.arg(3),
    );
    let arg_values = (0..arg_count)
        .map(|index| {
            p.mem
                .read_u64(args_pointer.wrapping_add(index as u64 * 8))
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    let caller = p.mem.read_u64(p.cpu.gpr[RSP]).unwrap_or(0);
    if p.trace_imports {
        let args = arg_values
            .iter()
            .map(|arg| format!("{arg:#x}"))
            .collect::<Vec<_>>()
            .join(", ");
        let stack = (0..72)
            .map(|index| {
                let offset = index * 8;
                let value = p.rd64(p.cpu.gpr[RSP].wrapping_add(offset)).unwrap_or(0);
                format!("+{offset:#04x}:{value:#x}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        p.stderr.extend_from_slice(
            format!(
                "[seh] RaiseException code={code:#010x} flags={flags:#x} arg_count={arg_count} args=[{args}] caller={caller:#x}\n"
            )
            .as_bytes(),
        );
        p.stderr.extend_from_slice(
            format!("[seh] stack rsp={:#x} words=[{stack}]\n", p.cpu.gpr[RSP]).as_bytes(),
        );
    }
    crate::exception::dispatch_raised_exception(p, code, flags, &arg_values, caller)
}

fn terminate_process(p: &mut Process) -> R {
    let (h, code) = (p.arg(0), p.arg(1) as u32);
    if h == u64::MAX || h == 0x1000 {
        p.finish_exit(code).map(|_| 0)
    } else {
        Ok(1)
    }
}
fn exit_thread(p: &mut Process) -> R {
    let code = p.arg(0) as u32;
    if p.current_thread_is_guest {
        Err(Stop::ThreadExit(code))
    } else {
        p.finish_exit(code).map(|_| 0)
    }
}

fn create_thread(p: &mut Process) -> R {
    let (start, parameter, flags, thread_id_ptr) = (p.arg(2), p.arg(3), p.arg(4) as u32, p.arg(5));
    if start == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "CreateThread from a nested synchronous guest callback cannot be scheduled"
                .into(),
        });
    }
    let id = p.next_thread_id;
    p.next_thread_id = p.next_thread_id.wrapping_add(4).max(0x1008);
    if thread_id_ptr != 0 {
        p.wr32(thread_id_ptr, id)?;
    }
    let suspended = flags & 4 != 0;
    let handle = p.new_handle(Handle::Thread {
        id,
        start,
        parameter,
        exit_code: None,
        suspended,
    });
    p.create_guest_thread(id, start, parameter, suspended)?;
    if !suspended {
        p.request_thread_yield();
    }
    Ok(handle)
}

fn resume_thread(p: &mut Process) -> R {
    let h = p.arg(0);
    let (id, suspended) = match p.handles.get(&h) {
        Some(Handle::Thread { id, suspended, .. }) => (*id, *suspended),
        _ => return Ok(u32::MAX as u64),
    };
    if !suspended {
        return Ok(0);
    }
    if !p.resume_guest_thread(id) {
        return Ok(u32::MAX as u64);
    }
    if let Some(Handle::Thread { suspended, .. }) = p.handles.get_mut(&h) {
        *suspended = false;
    }
    Ok(1)
}

fn sleep_thread(p: &mut Process, alertable: bool) -> R {
    let ms = p.arg(0) as u32;
    if ms == 0 {
        p.request_thread_yield();
        return Ok(0);
    }
    if alertable {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "alertable SleepEx requires APC delivery".into(),
        });
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "blocking Sleep from a nested synchronous guest callback cannot be scheduled"
                .into(),
        });
    }
    let duration = (ms as u64).saturating_mul(100_000);
    p.block_current_thread(ThreadWait::Sleep {
        deadline: p.now_ticks().saturating_add(duration),
    });
    Ok(0)
}

fn enter_critical_section(p: &mut Process) -> R {
    let address = p.arg(0);
    let id = p.current_thread_id;
    if p.try_acquire_critical(address, id) {
        return Ok(0);
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "contended critical section from a nested synchronous guest callback cannot be scheduled".into(),
        });
    }
    let waiters = &mut p.critical_sections.entry(address).or_default().waiters;
    if !waiters.contains(&id) {
        waiters.push_back(id);
    }
    p.block_current_thread(ThreadWait::CriticalAcquire { address });
    Ok(0)
}

fn acquire_srw(p: &mut Process, exclusive: bool) -> R {
    let address = p.arg(0);
    let id = p.current_thread_id;
    if p.try_acquire_srw(address, id, exclusive) {
        return Ok(0);
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "contended SRW lock from a nested synchronous guest callback cannot be scheduled"
                .into(),
        });
    }
    let waiters = &mut p.srw_locks.entry(address).or_default().waiters;
    if !waiters.contains(&id) {
        waiters.push_back(id);
    }
    p.block_current_thread(ThreadWait::SrwAcquire { address, exclusive });
    Ok(0)
}

fn create_mutex(p: &mut Process) -> R {
    let owned = p.arg(1) != 0;
    Ok(p.new_handle(Handle::Mutex {
        owner: owned.then_some(p.current_thread_id),
        recursion: owned as u32,
    }))
}

fn create_mutex_ex(p: &mut Process) -> R {
    let owned = p.arg(2) & 1 != 0;
    Ok(p.new_handle(Handle::Mutex {
        owner: owned.then_some(p.current_thread_id),
        recursion: owned as u32,
    }))
}

fn create_semaphore(p: &mut Process) -> R {
    let (initial, max_count) = (p.arg(1) as u32, p.arg(2) as u32);
    if max_count == 0 || initial > max_count {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    Ok(p.new_handle(Handle::Semaphore {
        count: initial,
        max_count,
    }))
}

fn create_semaphore_ex(p: &mut Process) -> R {
    let (initial, max_count) = (p.arg(1) as u32, p.arg(2) as u32);
    if max_count == 0 || initial > max_count {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    Ok(p.new_handle(Handle::Semaphore {
        count: initial,
        max_count,
    }))
}

fn release_mutex(p: &mut Process) -> R {
    let handle = p.arg(0);
    let id = p.current_thread_id;
    match p.handles.get_mut(&handle) {
        Some(Handle::Mutex { owner, recursion }) if *owner == Some(id) => {
            *recursion -= 1;
            if *recursion == 0 {
                *owner = None;
            }
            Ok(1)
        }
        Some(Handle::Mutex { .. }) => {
            p.set_last_error(ERROR_NOT_OWNER);
            Ok(0)
        }
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn release_semaphore(p: &mut Process) -> R {
    let (handle, release_count, previous_count) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    let result = match p.handles.get_mut(&handle) {
        Some(Handle::Semaphore { count, max_count })
            if release_count != 0 && count.saturating_add(release_count) <= *max_count =>
        {
            let previous = *count;
            *count += release_count;
            Some(previous)
        }
        Some(Handle::Semaphore { .. }) => None,
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(0);
        }
    };
    let Some(previous) = result else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    if previous_count != 0 {
        p.wr32(previous_count, previous)?;
    }
    Ok(1)
}

fn wait_for_single_object(p: &mut Process, alertable: bool) -> R {
    let (handle, timeout) = (p.arg(0), p.arg(1) as u32);
    match p.try_wait_handles(&[handle], false, p.current_thread_id) {
        Ok(Some(_)) => return Ok(WAIT_OBJECT_0),
        Err(error) => {
            p.set_last_error(error);
            return Ok(WAIT_FAILED);
        }
        _ => {}
    }
    if timeout == 0 {
        return Ok(WAIT_TIMEOUT);
    }
    if alertable {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "alertable WaitForSingleObjectEx requires APC delivery".into(),
        });
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "blocking WaitForSingleObject from a nested synchronous guest callback cannot be scheduled".into(),
        });
    }
    let deadline = (timeout != u32::MAX).then(|| {
        p.now_ticks()
            .saturating_add((timeout as u64).saturating_mul(100_000))
    });
    p.block_current_thread(ThreadWait::Handle { handle, deadline });
    Ok(WAIT_TIMEOUT)
}

fn wait_for_multiple_objects(p: &mut Process, alertable: bool) -> R {
    let count = p.arg(0) as usize;
    let handles = p.arg(1);
    if count == 0 || count > 64 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(WAIT_FAILED);
    }
    let wait_all = p.arg(2) != 0;
    let timeout = p.arg(3) as u32;
    let mut wait_handles = Vec::with_capacity(count);
    for i in 0..count {
        wait_handles.push(p.rd64(handles + (i as u64) * 8)?);
    }
    match p.try_wait_handles(&wait_handles, wait_all, p.current_thread_id) {
        Ok(Some(index)) => return Ok(WAIT_OBJECT_0 + index),
        Err(error) => {
            p.set_last_error(error);
            return Ok(WAIT_FAILED);
        }
        _ => {}
    }
    if timeout == 0 {
        return Ok(WAIT_TIMEOUT);
    }
    if alertable {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "alertable WaitForMultipleObjectsEx requires APC delivery".into(),
        });
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "blocking WaitForMultipleObjects from a nested synchronous guest callback cannot be scheduled".into(),
        });
    }
    let deadline = (timeout != u32::MAX).then(|| {
        p.now_ticks()
            .saturating_add((timeout as u64).saturating_mul(100_000))
    });
    p.block_current_thread(ThreadWait::Multiple {
        handles: wait_handles,
        wait_all,
        deadline,
    });
    Ok(WAIT_TIMEOUT)
}

fn sleep_condition_variable_cs(p: &mut Process) -> R {
    sleep_condition_variable(p, true)
}

fn sleep_condition_variable_srw(p: &mut Process) -> R {
    sleep_condition_variable(p, false)
}

fn sleep_condition_variable(p: &mut Process, critical: bool) -> R {
    let (condition, lock, timeout) = (p.arg(0), p.arg(1), p.arg(2) as u32);
    let flags = if critical { 0 } else { p.arg(3) as u32 };
    if p.trace_imports {
        let threads = p
            .handles
            .values()
            .filter_map(|handle| match handle {
                Handle::Thread {
                    id,
                    exit_code,
                    suspended,
                    ..
                } => Some(format!("{id}:{exit_code:?}:suspended={suspended}")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(",");
        p.stderr.extend_from_slice(format!(
            "[sync] SleepConditionVariableSRW cond={condition:#x} lock={lock:#x} timeout={timeout:#x} flags={flags:#x} tid={} nested={} threads=[{threads}]\n",
            p.current_thread_id, p.nested
        ).as_bytes());
    }
    let exclusive = flags == 0;
    if flags > 1 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if timeout == 0 {
        p.set_last_error(ERROR_TIMEOUT);
        return Ok(0);
    }
    if p.nested != 0 {
        return Err(Stop::Unimplemented {
            dll: "kernel32".into(),
            name: "blocking SleepConditionVariable from a nested synchronous guest callback cannot be scheduled".into(),
        });
    }
    let released = if critical {
        let id = p.current_thread_id;
        let recursion = p
            .critical_sections
            .get(&lock)
            .filter(|state| state.owner == Some(id))
            .map(|state| state.recursion);
        recursion == Some(1) && p.release_critical(lock, id)
    } else {
        p.release_srw(lock, p.current_thread_id, exclusive)
    };
    if !released {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let deadline = (timeout != u32::MAX).then(|| {
        p.now_ticks()
            .saturating_add((timeout as u64).saturating_mul(100_000))
    });
    p.block_current_thread(ThreadWait::Condition {
        condition,
        lock,
        critical,
        exclusive,
        deadline,
        outcome: None,
    });
    Ok(0)
}

fn is_processor_feature_present(p: &mut Process) -> R {
    // PF_XMMI_INSTRUCTIONS_AVAILABLE(6), PF_XMMI64 (10), PF_COMPARE_EXCHANGE_DOUBLE(2/14), PF_NX (12),
    // PF_SSE3 (13), PF_COMPARE_EXCHANGE128 (14), PF_SSSE3 (36), PF_SSE4_1 (37), PF_SSE4_2 (38)
    Ok(matches!(p.arg(0), 2 | 6 | 10 | 12 | 13 | 14 | 36 | 37 | 38) as u64)
}

fn get_system_info(p: &mut Process) -> R {
    let a = p.arg(0);
    let mut b = [0u8; 48];
    b[0..2].copy_from_slice(&9u16.to_le_bytes()); // PROCESSOR_ARCHITECTURE_AMD64
    b[4..8].copy_from_slice(&4096u32.to_le_bytes());
    b[8..16].copy_from_slice(&0x10000u64.to_le_bytes());
    b[16..24].copy_from_slice(&0x7fff_fffe_ffffu64.to_le_bytes());
    b[24..32].copy_from_slice(&1u64.to_le_bytes());
    b[32..36].copy_from_slice(&1u32.to_le_bytes());
    b[36..40].copy_from_slice(&8664u32.to_le_bytes());
    b[40..44].copy_from_slice(&0x10000u32.to_le_bytes());
    b[44..46].copy_from_slice(&6u16.to_le_bytes());
    p.write_bytes(a, &b)?;
    Ok(0)
}

fn global_memory_status_ex(p: &mut Process) -> R {
    let a = p.arg(0);
    let mut b = [0u8; 64];
    b[0..4].copy_from_slice(&64u32.to_le_bytes());
    b[4..8].copy_from_slice(&30u32.to_le_bytes());
    let total: u64 = 8 << 30;
    for (i, v) in [
        total,
        total / 2,
        total * 2,
        total,
        1u64 << 47,
        (1u64 << 47) - (1 << 30),
        0,
    ]
    .iter()
    .enumerate()
    {
        b[8 + 8 * i..16 + 8 * i].copy_from_slice(&v.to_le_bytes());
    }
    p.write_bytes(a, &b)?;
    Ok(1)
}

fn get_version_ex(p: &mut Process) -> R {
    let a = p.arg(0);
    let sz = p.rd32(a)?;
    p.wr32(a + 4, 10)?;
    p.wr32(a + 8, 0)?;
    p.wr32(a + 12, 19045)?;
    p.wr32(a + 16, 2)?;
    let _ = sz;
    Ok(1)
}

fn verify_version_info(p: &mut Process) -> R {
    const STATUS_INVALID_PARAMETER: u64 = 0xC000_000D;
    const STATUS_REVISION_MISMATCH: u64 = 0xC000_0059;

    let (version, type_mask, condition_mask) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if p.trace_imports {
        let (size, major, minor, build) = if version != 0 {
            (
                p.rd32(version)?,
                p.rd32(version + 4)?,
                p.rd32(version + 8)?,
                p.rd32(version + 12)?,
            )
        } else {
            (0, 0, 0, 0)
        };
        p.stderr.extend_from_slice(
            format!(
                "[version] VerifyVersionInfo info={version:#x} size={size} requested={major}.{minor}.{build} type={type_mask:#x} conditions={condition_mask:#018x}\n"
            )
            .as_bytes(),
        );
    }

    match ntdll::rtl_verify_version_info(p)? {
        0 => Ok(1),
        STATUS_REVISION_MISMATCH => {
            p.set_last_error(ERROR_OLD_WIN_VERSION);
            Ok(0)
        }
        STATUS_INVALID_PARAMETER => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            Ok(0)
        }
        _ => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            Ok(0)
        }
    }
}

// heap -------------------------------------------------------------------------------------

pub fn alloc_block(p: &mut Process, n: u64, zero: bool) -> u64 {
    let mut maps: Vec<(u64, u64)> = Vec::new();
    let a = p.heap.alloc(n, &mut |base, len| {
        maps.push((base, len));
        true
    });
    for (b, l) in maps {
        p.mem.map(b, l, PROT_RW);
    }
    match a {
        Some(a) => {
            if zero {
                let sz = p.heap.size_of(a).unwrap_or(n) as usize;
                let _ = p.mem.write_from(a, &vec![0u8; sz]);
            }
            a
        }
        None => 0,
    }
}

pub fn free_block(p: &mut Process, a: u64) -> bool {
    p.heap.free(a).is_some()
}

pub fn realloc_block(p: &mut Process, a: u64, n: u64, flags: u64) -> R {
    if a == 0 {
        return Ok(alloc_block(p, n, flags & 0x48 != 0));
    }
    let old = match p.heap.size_of(a) {
        Some(s) => s,
        None => return Ok(0),
    };
    if n <= old {
        return Ok(a);
    }
    let new = alloc_block(p, n, flags & 0x48 != 0);
    if new == 0 {
        return Ok(0);
    }
    let data = p.read_bytes(a, old as usize)?;
    p.write_bytes(new, &data)?;
    p.heap.free(a);
    Ok(new)
}

fn heap_alloc(p: &mut Process) -> R {
    let (fl, n) = (p.arg(1), p.arg(2));
    let a = alloc_block(p, n, true);
    let _ = fl;
    if a == 0 {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    }
    Ok(a)
}
fn heap_realloc(p: &mut Process) -> R {
    let (fl, a, n) = (p.arg(1), p.arg(2), p.arg(3));
    realloc_block(p, a, n, fl | 0x8)
}
fn heap_free(p: &mut Process) -> R {
    let a = p.arg(2);
    Ok((a == 0 || free_block(p, a)) as u64)
}
fn local_alloc(p: &mut Process) -> R {
    let (fl, n) = (p.arg(0), p.arg(1));
    Ok(alloc_block(p, n, fl & 0x40 != 0 || true))
}

// virtual memory ---------------------------------------------------------------------------

fn prot_from_win(w: u64) -> u8 {
    match w & 0xff {
        0x01 => PROT_NONE,
        0x02 => PROT_R,
        0x04 | 0x08 => PROT_RW,
        0x10 => PROT_X,
        0x20 => PROT_R | PROT_X,
        0x40 | 0x80 => PROT_RWX,
        _ => PROT_RW,
    }
}
fn prot_to_win(p: u8) -> u32 {
    match p {
        0 => 0x01,
        1 => 0x02,
        3 => 0x04,
        4 => 0x10,
        5 => 0x20,
        7 => 0x40,
        2 => 0x04,
        6 => 0x40,
        _ => 0x04,
    }
}

fn virtual_alloc(p: &mut Process) -> R {
    let (addr, size, ty, prot) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if size == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let pr = prot_from_win(prot);
    // Commit of an already-mapped region: just (re)protect it.
    if addr != 0 && p.mem.is_mapped(addr) {
        if ty & 0x1000 != 0 {
            let len = (size + 0xfff) & !0xfff;
            let end = (addr & !0xfff) + len;
            let mut a = addr & !0xfff;
            while a < end {
                if !p.mem.is_mapped(a) {
                    p.mem.map(a, 0x1000, pr);
                }
                a += 0x1000;
            }
            p.mem.protect(addr & !0xfff, len, pr);
        }
        return Ok(addr);
    }
    if addr != 0 {
        let a = addr & !0xfff;
        let len = ((addr - a) + size + 0xfff) & !0xfff;
        if p.mem.find_free(a, len) != Some(a) {
            p.set_last_error(487);
            return Ok(0);
        }
        p.mem
            .map(a, len, if ty & 0x1000 != 0 { pr } else { PROT_NONE });
        p.vmem.insert(a, (len, pr));
        return Ok(a);
    }
    let mapped_prot = if ty & 0x1000 != 0 { pr } else { PROT_RW };
    Ok(p.alloc_virtual(size, mapped_prot))
}

fn virtual_free(p: &mut Process) -> R {
    let (addr, size, ty) = (p.arg(0), p.arg(1), p.arg(2));
    if ty & 0x8000 != 0 {
        if let Some((len, _)) = p.vmem.remove(&addr) {
            p.mem.unmap(addr, len);
            return Ok(1);
        }
        return Ok(0);
    }
    if ty & 0x4000 != 0 && size > 0 {
        // MEM_DECOMMIT: keep the mapping (zero it) for simplicity.
        return Ok(1);
    }
    Ok(1)
}

fn virtual_protect(p: &mut Process) -> R {
    let (addr, size, new, old) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    let a = addr & !0xfff;
    let len = ((addr - a) + size + 0xfff) & !0xfff;
    let prev = match p.mem.prot_of(a) {
        Some(x) => x,
        None => {
            p.set_last_error(487);
            return Ok(0);
        }
    };
    if old != 0 {
        p.wr32(old, prot_to_win(prev))?;
    }
    Ok(p.mem.protect(a, len, prot_from_win(new)) as u64)
}

fn virtual_query(p: &mut Process) -> R {
    let (addr, out, len) = (p.arg(0), p.arg(1), p.arg(2));
    if len < 48 {
        return Ok(0);
    }
    let page = addr & !0xfff;
    let mut b = [0u8; 48];
    match p.mem.prot_of(page) {
        Some(pr) => {
            // Extend over pages with identical protection.
            let mut start = page;
            while start >= 0x1000 && p.mem.prot_of(start - 0x1000) == Some(pr) {
                start -= 0x1000;
            }
            let mut end = page + 0x1000;
            while p.mem.prot_of(end) == Some(pr) {
                end += 0x1000;
            }
            let alloc_base = p
                .modules
                .iter()
                .find(|m| page >= m.base && page < m.base + m.size)
                .map(|m| m.base)
                .unwrap_or(start);
            b[0..8].copy_from_slice(&start.to_le_bytes());
            b[8..16].copy_from_slice(&alloc_base.to_le_bytes());
            b[16..20].copy_from_slice(&prot_to_win(pr).to_le_bytes());
            b[24..32].copy_from_slice(&(end - start).to_le_bytes());
            b[32..36].copy_from_slice(&0x1000u32.to_le_bytes()); // MEM_COMMIT
            b[36..40].copy_from_slice(&prot_to_win(pr).to_le_bytes());
            let is_image = p
                .modules
                .iter()
                .any(|m| page >= m.base && page < m.base + m.size);
            b[40..44]
                .copy_from_slice(&(if is_image { 0x1000000u32 } else { 0x20000 }).to_le_bytes());
        }
        None => {
            b[0..8].copy_from_slice(&page.to_le_bytes());
            b[24..32].copy_from_slice(&0x1000u64.to_le_bytes());
            b[32..36].copy_from_slice(&0x10000u32.to_le_bytes()); // MEM_FREE
            b[36..40].copy_from_slice(&0x01u32.to_le_bytes());
        }
    }
    p.write_bytes(out, &b)?;
    Ok(48)
}

// files / console --------------------------------------------------------------------------

fn emit(p: &mut Process, handle: u64, data: &[u8]) -> Option<usize> {
    match handle {
        STD_OUT => {
            p.stdout.extend_from_slice(data);
            Some(data.len())
        }
        STD_ERR => {
            p.stderr.extend_from_slice(data);
            Some(data.len())
        }
        h => match p.handles.get_mut(&h) {
            Some(Handle::File {
                path,
                pos,
                write: true,
            }) => {
                let path = path.clone();
                let at = *pos;
                *pos += data.len();
                let f = p.vfs.get_mut(&path)?;
                if f.len() < at + data.len() {
                    f.resize(at + data.len(), 0);
                }
                f[at..at + data.len()].copy_from_slice(data);
                Some(data.len())
            }
            _ => None,
        },
    }
}

fn write_file(p: &mut Process) -> R {
    let (h, buf, n, written) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    let data = p.read_bytes(buf, n)?;
    match emit(p, h, &data) {
        Some(k) => {
            if written != 0 {
                p.wr32(written, k as u32)?;
            }
            Ok(1)
        }
        None => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn read_file(p: &mut Process) -> R {
    let (h, buf, n, read) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    let data: Vec<u8> = if h == STD_IN {
        let k = n.min(p.stdin.len());
        p.stdin.drain(..k).collect()
    } else if let Some(Handle::File { path, pos, .. }) = p.handles.get(&h) {
        let (path, at) = (path.clone(), *pos);
        let f = p.vfs.get(&path).cloned().unwrap_or_default();
        let end = (at + n).min(f.len());
        let d = if at < f.len() {
            f[at..end].to_vec()
        } else {
            Vec::new()
        };
        if let Some(Handle::File { pos, .. }) = p.handles.get_mut(&h) {
            *pos += d.len();
        }
        d
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    p.write_bytes(buf, &data)?;
    if read != 0 {
        p.wr32(read, data.len() as u32)?;
    }
    Ok(1)
}

fn write_console_a(p: &mut Process) -> R {
    let (h, buf, n, w) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    let data = p.read_bytes(buf, n)?;
    emit(p, h, &data);
    if w != 0 {
        p.wr32(w, n as u32)?;
    }
    Ok(1)
}

fn write_console_w(p: &mut Process) -> R {
    let (h, buf, n, w) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    let raw = p.read_bytes(buf, n * 2)?;
    let u: Vec<u16> = raw
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&u);
    emit(p, h, s.as_bytes());
    if w != 0 {
        p.wr32(w, n as u32)?;
    }
    Ok(1)
}

fn get_console_screen_buffer_info(p: &mut Process) -> R {
    let a = p.arg(1);
    let mut b = [0u8; 22];
    let v: [u16; 11] = [120, 30, 0, 0, 7, 0, 0, 119, 29, 120, 30];
    for (i, x) in v.iter().enumerate() {
        b[2 * i..2 * i + 2].copy_from_slice(&x.to_le_bytes());
    }
    p.write_bytes(a, &b)?;
    Ok(1)
}

fn create_file(p: &mut Process, wide: bool) -> R {
    let (name, access, _share, _sec, disp) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3), p.arg(4));
    let raw_path = if wide {
        p.read_wstr(name)?
    } else {
        p.read_cstr(name)?
    };
    let n = norm(&raw_path);
    if n == "conout$" {
        return Ok(STD_OUT);
    }
    if n == "conin$" {
        return Ok(STD_IN);
    }
    let path = absolute(&raw_path, &p.current_directory);
    let write = access & 0x4000_0000 != 0 || access & 0x1_0000 != 0 || access & 2 != 0;
    let exists = p.vfs.exists(&path);
    match disp {
        1 if exists => {
            p.set_last_error(ERROR_ALREADY_EXISTS);
            return Ok(INVALID_HANDLE);
        }
        1 | 2 => p.vfs.insert(&path, Vec::new()),
        3 | 5 if !exists => {
            p.set_last_error(ERROR_FILE_NOT_FOUND);
            return Ok(INVALID_HANDLE);
        }
        4 if !exists => p.vfs.insert(&path, Vec::new()),
        _ => {}
    }
    if disp == 5 {
        p.vfs.insert(&path, Vec::new());
    }
    p.set_last_error(0);
    Ok(p.new_handle(Handle::File {
        path: norm(&path),
        pos: 0,
        write,
    }))
}

fn file_len(p: &Process, h: u64) -> Option<u64> {
    match p.handles.get(&h) {
        Some(Handle::File { path, .. }) => p.vfs.get(path).map(|f| f.len() as u64),
        _ => None,
    }
}

fn file_attrs(p: &Process, path: &str) -> u64 {
    let path = absolute(path, &p.current_directory);
    if p.vfs.exists(&path) {
        0x80
    } else if p.vfs.is_dir(&path) || matches!(norm(&path).as_str(), "windows" | "temp") {
        0x10
    } else {
        0xffff_ffff
    }
}

fn create_directory(p: &mut Process, path: &str) -> R {
    if path.is_empty() {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let absolute_path = absolute(path, &p.current_directory);
    match p.vfs.create_dir(&absolute_path) {
        Ok(()) => {
            p.set_last_error(0);
            Ok(1)
        }
        Err(CreateDirError::AlreadyExists) => {
            p.set_last_error(ERROR_ALREADY_EXISTS);
            Ok(0)
        }
        Err(CreateDirError::PathNotFound) => {
            p.set_last_error(ERROR_PATH_NOT_FOUND);
            Ok(0)
        }
    }
}

fn get_current_directory(p: &mut Process, wide: bool) -> R {
    let (capacity, buffer) = (p.arg(0) as usize, p.arg(1));
    let current_directory = p.current_directory.clone();
    let length = if wide {
        current_directory.encode_utf16().count()
    } else {
        current_directory.len()
    };
    let required = length + 1;
    if capacity < required {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(required as u64);
    }
    if wide {
        p.write_wstr(buffer, &current_directory)?;
    } else {
        p.write_cstr(buffer, &current_directory)?;
    }
    p.set_last_error(0);
    Ok(length as u64)
}

fn set_current_directory(p: &mut Process, path: &str) -> R {
    if path.is_empty() {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let absolute_path = absolute(path, &p.current_directory);
    if !p.vfs.is_dir(&absolute_path) {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    p.current_directory = absolute_path;
    p.set_last_error(0);
    Ok(1)
}

fn set_file_pointer(p: &mut Process) -> R {
    let (h, lo, hi_ptr, method) = (p.arg(0), p.arg(1) as u32 as i32 as i64, p.arg(2), p.arg(3));
    let hi = if hi_ptr != 0 {
        (p.rd32(hi_ptr)? as i32 as i64) << 32
    } else {
        0
    };
    let off = if hi_ptr != 0 {
        hi | (lo as u64 & 0xffff_ffff) as i64
    } else {
        lo
    };
    let new = seek(p, h, off, method)?;
    match new {
        Some(n) => {
            if hi_ptr != 0 {
                p.wr32(hi_ptr, (n >> 32) as u32)?;
            }
            Ok(n & 0xffff_ffff)
        }
        None => Ok(0xffff_ffff),
    }
}
fn set_file_pointer_ex(p: &mut Process) -> R {
    let (h, off, out, method) = (p.arg(0), p.arg(1) as i64, p.arg(2), p.arg(3));
    match seek(p, h, off, method)? {
        Some(n) => {
            if out != 0 {
                p.wr64(out, n)?;
            }
            Ok(1)
        }
        None => Ok(0),
    }
}
fn seek(p: &mut Process, h: u64, off: i64, method: u64) -> Result<Option<u64>, Stop> {
    let len = file_len(p, h);
    match (p.handles.get_mut(&h), len) {
        (Some(Handle::File { pos, .. }), Some(len)) => {
            let base = match method {
                0 => 0i64,
                1 => *pos as i64,
                _ => len as i64,
            };
            let n = (base + off).max(0) as usize;
            *pos = n;
            Ok(Some(n as u64))
        }
        _ => Ok(None),
    }
}

fn full_path(p: &mut Process, wide: bool) -> R {
    let (name, n, buf) = (p.arg(0), p.arg(1) as usize, p.arg(2));
    let path = if wide {
        p.read_wstr(name)?
    } else {
        p.read_cstr(name)?
    };
    let full = absolute(&path, &p.current_directory);
    let length = if wide {
        full.encode_utf16().count()
    } else {
        full.len()
    };
    if length + 1 <= n {
        if wide {
            p.write_wstr(buf, &full)?;
        } else {
            p.write_cstr(buf, &full)?;
        }
        if p.arg(3) != 0 {
            let file_part = full.rfind('\\').map(|i| i + 1).unwrap_or(0);
            if wide {
                p.wr64(
                    p.arg(3),
                    buf + (full[..file_part].encode_utf16().count() * 2) as u64,
                )?;
            } else {
                p.wr64(p.arg(3), buf + file_part as u64)?;
            }
        }
        Ok(length as u64)
    } else {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        Ok(length as u64 + 1)
    }
}

fn module_file_name(p: &mut Process, wide: bool) -> R {
    let (h, buf, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
    let path = if h == 0 {
        p.image_path.clone()
    } else {
        match p.module_by_base(h) {
            Some(i) => p.modules[i].path.clone(),
            None => {
                p.set_last_error(ERROR_MOD_NOT_FOUND);
                return Ok(0);
            }
        }
    };
    let len = path.len();
    let w = len.min(n.saturating_sub(1));
    let s = &path[..w];
    if wide {
        p.write_wstr(buf, s)?;
    } else {
        p.write_cstr(buf, s)?;
    }
    if len >= n {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
    }
    Ok(w as u64)
}

fn get_command_line_a(p: &mut Process) -> R {
    if p.extra.cmdline_a == 0 {
        let s = p.cmdline.clone();
        let a = alloc_block(p, s.len() as u64 + 1, true);
        p.write_cstr(a, &s)?;
        p.extra.cmdline_a = a;
    }
    Ok(p.extra.cmdline_a)
}
fn get_command_line_w(p: &mut Process) -> R {
    if p.extra.cmdline_w == 0 {
        let s = p.cmdline.clone();
        let a = alloc_block(p, (s.len() as u64 + 1) * 2, true);
        p.write_wstr(a, &s)?;
        p.extra.cmdline_w = a;
    }
    Ok(p.extra.cmdline_w)
}

fn env_block(p: &Process) -> Vec<String> {
    p.env.iter().map(|(k, v)| format!("{k}={v}")).collect()
}
fn get_environment_strings_w(p: &mut Process) -> R {
    let mut buf: Vec<u8> = Vec::new();
    for s in env_block(p) {
        buf.extend(s.encode_utf16().flat_map(|c| c.to_le_bytes()));
        buf.extend([0, 0]);
    }
    buf.extend([0, 0]);
    let a = alloc_block(p, buf.len() as u64, true);
    p.write_bytes(a, &buf)?;
    Ok(a)
}
fn get_environment_strings_a(p: &mut Process) -> R {
    let mut buf: Vec<u8> = Vec::new();
    for s in env_block(p) {
        buf.extend(s.bytes());
        buf.push(0);
    }
    buf.push(0);
    let a = alloc_block(p, buf.len() as u64, true);
    p.write_bytes(a, &buf)?;
    Ok(a)
}
fn env_var(p: &mut Process, wide: bool) -> R {
    let (name, buf, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
    let k = if wide {
        p.read_wstr(name)?
    } else {
        p.read_cstr(name)?
    };
    let v = p
        .env
        .iter()
        .find(|(a, _)| a.eq_ignore_ascii_case(&k))
        .map(|(_, v)| v.clone());
    match v {
        Some(v) => {
            if v.len() + 1 <= n {
                if wide {
                    p.write_wstr(buf, &v)?;
                } else {
                    p.write_cstr(buf, &v)?;
                }
                Ok(v.len() as u64)
            } else {
                Ok(v.len() as u64 + 1)
            }
        }
        None => {
            p.set_last_error(203);
            Ok(0)
        }
    }
}

// modules ----------------------------------------------------------------------------------

fn module_handle(p: &mut Process, name: Option<String>) -> R {
    match name {
        None => Ok(p
            .modules
            .iter()
            .find(|m| m.hle_group.is_none() && !m.is_dll)
            .map(|m| m.base)
            .unwrap_or(0)),
        Some(n) => {
            let n = if n.contains('.') {
                n
            } else {
                format!("{n}.dll")
            };
            if let Some(i) = p.module_by_name(&n) {
                return Ok(p.modules[i].base);
            }
            // Well-known system libraries are always "loaded".
            if let Some(g) = group_of(&anywasm_pe::canonical_dll(crate::process::base_name(&n))) {
                let i = p.register_hle_module(&n, g);
                return Ok(p.modules[i].base);
            }
            p.set_last_error(ERROR_MOD_NOT_FOUND);
            Ok(0)
        }
    }
}

fn load_library(p: &mut Process, name: &str) -> R {
    let n = if name.contains('.') {
        name.to_string()
    } else {
        format!("{name}.dll")
    };
    if let Some(i) = p.module_by_name(&n) {
        return Ok(p.modules[i].base);
    }
    let relative = absolute(&n, &p.current_directory);
    let file = if p.vfs.exists(&relative) {
        Some(norm(&relative))
    } else if p.vfs.exists(&n) {
        Some(norm(&n))
    } else {
        p.vfs.find_by_name(crate::process::base_name(&n)).cloned()
    };
    if let Some(f) = file {
        let data = p.vfs.get(&f).unwrap().clone();
        let before = p.modules.len();
        return match p.load_image(&data, &f, true) {
            Ok(i) => {
                // Run DllMain for the new module and any dependencies it pulled in.
                for k in before..p.modules.len() {
                    let (base, entry, hle) = (
                        p.modules[k].base,
                        p.modules[k].entry,
                        p.modules[k].hle_group.is_some(),
                    );
                    if !hle && entry != 0 {
                        p.call_guest(entry, &[base, 1, 0])?;
                    }
                }
                Ok(p.modules[i].base)
            }
            Err(e) => {
                p.stderr
                    .extend_from_slice(format!("LoadLibrary({n}): {e}\n").as_bytes());
                p.set_last_error(193);
                Ok(0)
            }
        };
    }
    if let Some(g) = group_of(&anywasm_pe::canonical_dll(crate::process::base_name(&n))) {
        let i = p.register_hle_module(&n, g);
        return Ok(p.modules[i].base);
    }
    p.set_last_error(ERROR_MOD_NOT_FOUND);
    Ok(0)
}

fn get_proc_address(p: &mut Process) -> R {
    let (h, name_ptr) = (p.arg(0), p.arg(1));
    let Some(i) = p.module_by_base(h).or_else(|| {
        if h == 0 {
            None
        } else {
            p.modules
                .iter()
                .position(|m| h >= m.base && h < m.base + m.size)
        }
    }) else {
        p.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(0);
    };
    let m = p.modules[i].clone();
    let by_ordinal = name_ptr < 0x10000;
    let name = if by_ordinal {
        format!("#{name_ptr}")
    } else {
        p.read_cstr(name_ptr)?
    };
    if let Some(g) = m.hle_group {
        if crate::hle::lookup(g, &name).is_some() {
            return Ok(p.thunk_for(g, &name, true).unwrap());
        }
        return match p.thunk_for(g, &name, false) {
            Some(a) => Ok(a),
            None => {
                p.set_last_error(ERROR_PROC_NOT_FOUND);
                p.unimplemented_log
                    .push((format!("{} (GetProcAddress)", m.name), name));
                Ok(0)
            }
        };
    }
    let a = if by_ordinal {
        m.ordinals.get(&(name_ptr as u16)).copied()
    } else {
        m.exports.get(&name).copied()
    };
    match a {
        Some(a) => Ok(a),
        None => {
            p.set_last_error(ERROR_PROC_NOT_FOUND);
            Ok(0)
        }
    }
}

// sync -------------------------------------------------------------------------------------

fn init_once_execute_once(p: &mut Process) -> R {
    let (once, func, param, ctx) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if p.rd64(once)? == 0 {
        let r = p.call_guest(func, &[once, param, ctx])?;
        if r & 1 == 0 {
            return Ok(0);
        }
        p.wr64(once, 1)?;
    }
    Ok(1)
}

fn interlocked(p: &mut Process, width: u8, f: impl Fn(u64, u64) -> u64, return_new: bool) -> R {
    let (addr, arg) = (p.arg(0), p.arg(1));
    let old = if width == 4 {
        p.rd32(addr)? as u64
    } else {
        p.rd64(addr)?
    };
    let new = f(old, arg);
    if width == 4 {
        p.wr32(addr, new as u32)?;
    } else {
        p.wr64(addr, new)?;
    }
    let r = if return_new { new } else { old };
    Ok(if width == 4 { r & 0xffff_ffff } else { r })
}

fn cmpxchg(p: &mut Process, width: u8) -> R {
    let (addr, new, cmp) = (p.arg(0), p.arg(1), p.arg(2));
    let old = if width == 4 {
        p.rd32(addr)? as u64
    } else {
        p.rd64(addr)?
    };
    if old == if width == 4 { cmp & 0xffff_ffff } else { cmp } {
        if width == 4 {
            p.wr32(addr, new as u32)?;
        } else {
            p.wr64(addr, new)?;
        }
    }
    Ok(old)
}

// time -------------------------------------------------------------------------------------

fn get_system_time(p: &mut Process) -> R {
    let a = p.arg(0);
    let ms_total = ticks(p) / 100_000;
    let secs = ms_total / 1000;
    let v: [u16; 8] = [
        2026,
        1,
        4,
        1,
        (secs / 3600 % 24) as u16,
        (secs / 60 % 60) as u16,
        (secs % 60) as u16,
        (ms_total % 1000) as u16,
    ];
    let mut b = [0u8; 16];
    for (i, x) in v.iter().enumerate() {
        b[2 * i..2 * i + 2].copy_from_slice(&x.to_le_bytes());
    }
    p.write_bytes(a, &b)?;
    Ok(0)
}

// strings ----------------------------------------------------------------------------------

fn read_wide_n(p: &Process, a: u64, n: i64) -> Result<Vec<u16>, Stop> {
    if n < 0 {
        Ok(p.read_wstr(a)?.encode_utf16().collect())
    } else {
        let raw = p.read_bytes(a, n as usize * 2)?;
        Ok(raw
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect())
    }
}

fn cp_decode(cp: u32, bytes: &[u8]) -> Vec<u16> {
    match cp {
        65001 => String::from_utf8_lossy(bytes).encode_utf16().collect(),
        _ => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

pub fn cp1252(b: u8) -> u16 {
    const HI: [u16; 32] = [
        0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039,
        0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
        0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178,
    ];
    if (0x80..0xa0).contains(&b) {
        HI[(b - 0x80) as usize]
    } else {
        b as u16
    }
}

fn multi_byte_to_wide_char(p: &mut Process) -> R {
    // Both lengths are Win32 ints; upper bits in their x64 argument slots are unspecified.
    let (cp, _fl, src, n, dst, dn) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    if n == 0 || n < -1 || dn < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let bytes = if n < 0 {
        let mut b = p
            .mem
            .read_cstr(src, 1 << 20)
            .map_err(|e| Stop::Crash(format!("{e:?}")))?;
        b.push(0);
        b
    } else {
        p.read_bytes(src, n as usize)?
    };
    let w = cp_decode(cp, &bytes);
    if dn == 0 {
        return Ok(w.len() as u64);
    }
    if dst == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if w.len() > dn as usize {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    let raw: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    p.write_bytes(dst, &raw)?;
    Ok(w.len() as u64)
}

fn wide_char_to_multi_byte(p: &mut Process) -> R {
    // Both length parameters are Win32 ints, not pointer-sized values.
    let (cp, _fl, src, n, dst, dn) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    if n == 0 || n < -1 || dn < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut w = read_wide_n(p, src, n as i64)?;
    if n < 0 {
        w.push(0);
    }
    let out: Vec<u8> = if cp == 65001 {
        String::from_utf16_lossy(&w).into_bytes()
    } else {
        w.iter()
            .map(|&c| {
                if c < 0x100 && !(0x80..0xa0).contains(&c) {
                    c as u8
                } else {
                    (0x80u8..0xa0).find(|&b| cp1252(b) == c).unwrap_or(b'?')
                }
            })
            .collect()
    };
    if dn == 0 {
        return Ok(out.len() as u64);
    }
    if out.len() > dn as usize {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    p.write_bytes(dst, &out)?;
    Ok(out.len() as u64)
}

fn get_string_type_w(p: &mut Process) -> R {
    // (type, src, n, out) for GetStringTypeW; GetStringTypeExW/A shift args by one - handle W only.
    let (ty, src, n, out) = (p.arg(0), p.arg(1), p.arg(2) as i64, p.arg(3));
    let w = read_wide_n(p, src, n)?;
    let _ = ty;
    for (i, c) in w.iter().enumerate() {
        let ch = char::from_u32(*c as u32).unwrap_or('\0');
        let mut t = 0u16;
        if ch.is_uppercase() {
            t |= 0x1;
        }
        if ch.is_lowercase() {
            t |= 0x2;
        }
        if ch.is_ascii_digit() {
            t |= 0x4;
        }
        if ch.is_whitespace() {
            t |= 0x8;
        }
        if ch.is_ascii_punctuation() {
            t |= 0x10;
        }
        if ch.is_control() {
            t |= 0x20;
        }
        if ch == ' ' {
            t |= 0x40;
        }
        if ch.is_ascii_hexdigit() {
            t |= 0x80;
        }
        if ch.is_alphabetic() {
            t |= 0x100;
        }
        p.wr16(out + i as u64 * 2, t)?;
    }
    Ok(1)
}

fn lc_map_string_w(p: &mut Process) -> R {
    // cchSrc and cchDest are Win32 ints; their unused upper bits may be dirty in 64-bit ABI slots.
    let (flags, src, n, dst, dn) = (
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    if n == 0 || n < -1 || dn < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut w = read_wide_n(p, src, n as i64)?;
    if n < 0 {
        w.push(0);
    }
    if dn == 0 {
        return Ok(w.len() as u64);
    }
    for c in w.iter_mut() {
        let ch = char::from_u32(*c as u32).unwrap_or('\0');
        if flags & 0x200 != 0 {
            *c = ch
                .to_uppercase()
                .next()
                .map(|x| x as u32 as u16)
                .unwrap_or(*c);
        } else if flags & 0x100 != 0 {
            *c = ch
                .to_lowercase()
                .next()
                .map(|x| x as u32 as u16)
                .unwrap_or(*c);
        }
    }
    if dst == 0 || w.len() > dn as usize {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    let raw: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    p.write_bytes(dst, &raw)?;
    Ok(w.len() as u64)
}

#[cfg(test)]
mod lc_map_string_tests {
    use super::*;
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    #[test]
    fn static_tls_tlsalloc_and_flsalloc_use_separate_index_spaces() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let tls_alloc = lookup("TlsAlloc").unwrap();
        let fls_alloc = lookup("FlsAlloc").unwrap();
        assert_eq!(tls_alloc(&mut p), Ok(0));
        assert_eq!(fls_alloc(&mut p), Ok(0));
        assert_eq!(tls_alloc(&mut p), Ok(1));
        assert_eq!(fls_alloc(&mut p), Ok(1));
    }

    #[test]
    fn raise_exception_trace_reports_flags_arguments_and_guest_caller() {
        const BASE: u64 = 0x30000;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        let argument_array = BASE + 0x100;
        let stack = BASE + 0x800;
        p.wr64(argument_array, 0x1993_0520).unwrap();
        p.wr64(argument_array + 8, BASE + 0x300).unwrap();
        p.wr64(argument_array + 16, 0x5678).unwrap();
        p.wr64(argument_array + 24, 0x9abc).unwrap();
        p.wr64(stack, 0x1400_1234).unwrap();
        p.cpu.gpr[RSP] = stack;
        p.cpu.gpr[RCX] = 0xe06d_7363;
        p.cpu.gpr[RDX] = 1;
        p.cpu.gpr[R8] = 4;
        p.cpu.gpr[R9] = argument_array;
        p.trace_imports = true;

        assert_eq!(
            raise_exception(&mut p),
            Err(Stop::Crash(
                "MSVC C++ ThrowInfo 0x5678 is outside loaded image 0x9abc".into()
            ))
        );
        let trace = String::from_utf8(p.take_stderr()).unwrap();
        assert!(trace.starts_with(
            "[seh] RaiseException code=0xe06d7363 flags=0x1 arg_count=4 args=[0x19930520, 0x30300, 0x5678, 0x9abc] caller=0x14001234\n[seh] stack rsp=0x30800 words=[+0x00:0x14001234, +0x08:0x0"
        ));
        assert_eq!(trace.matches("+0x").count(), 72);
    }

    #[test]
    fn zero_capacity_query_ignores_unspecified_upper_stack_bits() {
        const BASE: u64 = 0x10000;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.write_wstr(BASE + 0x100, "abc").unwrap();

        p.cpu.gpr[RSP] = BASE + 0x800;
        p.cpu.gpr[RCX] = 0; // LCMapStringEx locale name
        p.cpu.gpr[RDX] = 0x200; // LCMAP_UPPERCASE
        p.cpu.gpr[R8] = BASE + 0x100;
        p.cpu.gpr[R9] = 3; // cchSrc
        p.wr64(BASE + 0x800 + 8 + 8 * 4, 0).unwrap(); // lpDestStr
        p.wr64(BASE + 0x800 + 8 + 8 * 5, 0xdead_beef_0000_0000)
            .unwrap(); // cchDest == 0

        assert_eq!(lc_map_string_w(&mut p), Ok(3));
    }

    #[test]
    fn wide_char_to_multi_byte_reads_signed_win32_lengths() {
        const BASE: u64 = 0x20000;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.write_wstr(BASE + 0x100, "abc").unwrap();

        p.cpu.gpr[RSP] = BASE + 0x800;
        p.cpu.gpr[RCX] = 1252;
        p.cpu.gpr[RDX] = 0;
        p.cpu.gpr[R8] = BASE + 0x100;
        p.cpu.gpr[R9] = 0xffff_ffff; // cchWideChar == -1, including the NUL terminator
        p.wr64(BASE + 0x800 + 8 + 8 * 4, 0).unwrap(); // lpMultiByteStr
        p.wr64(BASE + 0x800 + 8 + 8 * 5, 0x0000_7ff0_0000_0000)
            .unwrap(); // cbMultiByte == 0

        assert_eq!(wide_char_to_multi_byte(&mut p), Ok(4));
    }
}
