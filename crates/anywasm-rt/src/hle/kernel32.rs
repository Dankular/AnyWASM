//! kernel32 / kernelbase / api-ms-win-core-* host implementation.

use super::*;
use crate::process::*;
use crate::vfs::{absolute, norm, AssetReadError, CreateDirError, FileMetadata, RemoveDirError};
use anywasm_cpu::cpu::RSP;
use anywasm_cpu::mem::{PROT_NONE, PROT_R, PROT_RW, PROT_RWX, PROT_W, PROT_X};
use anywasm_cpu::CpuMode;

const INVALID_HANDLE: u64 = u64::MAX;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_NOT_SAME_DEVICE: u32 = 17;
const ERROR_READ_FAULT: u32 = 30;
const ERROR_LOCK_VIOLATION: u32 = 33;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_BUFFER_OVERFLOW: u32 = 111;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INVALID_FLAGS: u32 = 1004;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_RESOURCE_DATA_NOT_FOUND: u32 = 1812;
const ERROR_RESOURCE_TYPE_NOT_FOUND: u32 = 1813;
const ERROR_RESOURCE_NAME_NOT_FOUND: u32 = 1814;
const ERROR_RESOURCE_ENUM_USER_STOP: u32 = 15106;
const ERROR_TIMEOUT: u32 = 1460;
const ERROR_MOD_NOT_FOUND: u32 = 126;
const ERROR_PROC_NOT_FOUND: u32 = 127;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_FILE_EXISTS: u32 = 80;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_MR_MID_NOT_FOUND: u32 = 317;
const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_RESOURCE_LANG_NOT_FOUND: u32 = 1815;
const ERROR_NOT_OWNER: u32 = 288;
const ERROR_NOT_LOCKED: u32 = 158;
const ERROR_OLD_WIN_VERSION: u32 = 1150;
const THREAD_PRIORITY_ERROR_RETURN: u32 = 0x7fff_ffff;
const MAXIMUM_PROCESSORS_X86: u32 = 32;
const MAXIMUM_PROCESSORS_X64: u32 = 64;
const VIRTUAL_PROCESSOR_COUNT: u32 = 1;
// Captured with Windows GetStringTypeW for every UTF-16 code unit. The capture script lives in
// scripts/generate_windows_ctype1.ps1; keep the table and generator together when updating NLS.
const WINDOWS_CTYPE1: &[u8; 0x2_0000] = include_bytes!("windows_ctype1.bin");
// The runtime advertises Windows 10 Pro through its OS-version APIs. GetProductInfo maps that
// virtual SKU to the corresponding SKU identifier for the requested supported OS version.
const PRODUCT_BUSINESS: u32 = 0x0000_0006;
const PRODUCT_PROFESSIONAL: u32 = 0x0000_0030;
const WAIT_OBJECT_0: u64 = 0;
const WAIT_TIMEOUT: u64 = 0x102;
const WAIT_FAILED: u64 = u32::MAX as u64;
const CURRENT_PROCESS_ID: u32 = 0x1000;
const DUPLICATE_CLOSE_SOURCE: u32 = 1;
const DUPLICATE_SAME_ACCESS: u32 = 2;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const VIRTUAL_ANSI_CODE_PAGE: u32 = 1252;
pub const QUERY_PERFORMANCE_FREQUENCY: u64 = 10_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRangeLock {
    pub handle: u64,
    pub path: String,
    pub offset: u64,
    pub length: u64,
}

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
        // The guest process identity is fixed for this single-process virtual environment.
        "GetCurrentProcessId" => |_| Ok(CURRENT_PROCESS_ID as u64),
        "GetPriorityClass" => get_priority_class,
        "SetPriorityClass" => set_priority_class,
        "GetThreadPriority" => get_thread_priority,
        "SetThreadPriority" => set_thread_priority,
        "SetThreadExecutionState" => set_thread_execution_state,
        "SetThreadIdealProcessor" => set_thread_ideal_processor,
        "GetCurrentThreadId" => |p| Ok(p.current_thread_id as u64),
        "OpenThread" => open_thread,
        // The scheduler does not yet queue APC routines for alertable delivery. Report the
        // missing execution service explicitly instead of accepting an APC that can never run.
        "QueueUserAPC" => |_| {
            unsupported_kernel32(
                "QueueUserAPC",
                "APC queuing and alertable delivery are not modeled",
            )
        },
        "SuspendThread" => suspend_thread,
        "TerminateThread" => terminate_thread,
        "GetExitCodeProcess" => get_exit_code_process,
        "GetThreadId" => |p| {
            let h = p.arg(0);
            Ok(thread_id_from_handle(p, h).unwrap_or(0) as u64)
        },
        "GetExitCodeThread" => get_exit_code_thread,
        "GetProcessId" => get_process_id,
        "DuplicateHandle" => duplicate_handle,
        "IsBadWritePtr" => |p| Ok(is_bad_memory_range(p, p.arg(0), p.arg(1), PROT_W) as u64),
        "IsBadReadPtr" => |p| Ok(is_bad_memory_range(p, p.arg(0), p.arg(1), PROT_R) as u64),
        "IsBadCodePtr" => |p| Ok(is_bad_memory_range(p, p.arg(0), 1, PROT_X) as u64),
        "InitializeSListHead" => |p| {
            // SLIST_HEADER is 8 bytes on x86 and 16 bytes on Win64.
            let size = if p.cpu.mode == CpuMode::X86_32 { 8 } else { 16 };
            p.write_bytes(p.arg(0), &[0; 16][..size])?;
            Ok(0)
        },
        "GetLastError" => |p| {
            let offset = if p.cpu.mode == CpuMode::X86_32 {
                0x34
            } else {
                0x68
            };
            Ok(p.rd32(TEB_ADDR + offset)? as u64)
        },
        "GetPrivateProfileIntA" => get_private_profile_int_a,
        "GetPrivateProfileIntW" => get_private_profile_int_w,
        "SetLastError" => |p| {
            let v = p.arg(0) as u32;
            p.set_last_error(v);
            Ok(0)
        },
        "AddVectoredExceptionHandler" => add_vectored_exception_handler,
        "RemoveVectoredExceptionHandler" => remove_vectored_exception_handler,
        "AddVectoredContinueHandler" => add_vectored_continue_handler,
        "RemoveVectoredContinueHandler" => remove_vectored_continue_handler,
        // No debugger attachment is modeled by the runtime.
        "IsDebuggerPresent" => |_| Ok(0),
        "CheckRemoteDebuggerPresent" => check_remote_debugger_present,
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
        "SetUnhandledExceptionFilter" => set_unhandled_exception_filter,
        "UnhandledExceptionFilter" => crate::exception::unhandled_exception_filter,
        "RaiseException" => raise_exception,
        "RtlCaptureContext" => ntdll::rtl_capture_context,
        "IsProcessorFeaturePresent" => is_processor_feature_present,
        "GetSystemInfo" | "GetNativeSystemInfo" => get_system_info,
        "GlobalMemoryStatus" => global_memory_status,
        "GlobalMemoryStatusEx" => global_memory_status_ex,
        "GetVersion" => |_| Ok(0x4A61_0A00),
        "GetVersionExA" => |p| get_version_ex(p, false),
        "GetVersionExW" => |p| get_version_ex(p, true),
        "GetProductInfo" => get_product_info,
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
        "GetComputerNameA" => get_computer_name_a,
        "GetComputerNameW" => get_computer_name_w,
        "GetComputerNameExA" => get_computer_name_ex_a,
        // memory
        "GetProcessHeap" => |p| Ok(p.heap.process_heap),
        "HeapCreate" => heap_create,
        "HeapDestroy" => heap_destroy,
        "HeapAlloc" => heap_alloc,
        "HeapReAlloc" => heap_realloc,
        "HeapFree" => heap_free,
        "HeapSize" => heap_size,
        "HeapValidate" => heap_validate,
        "LocalAlloc" | "GlobalAlloc" => local_alloc,
        "GlobalSize" => global_size,
        "LocalFree" | "GlobalFree" => |p| {
            let a = p.arg(0);
            if a == 0 || free_block(p, a) {
                p.set_last_error(0);
                Ok(0)
            } else {
                p.set_last_error(ERROR_INVALID_HANDLE);
                Ok(a)
            }
        },
        "LocalReAlloc" | "GlobalReAlloc" => local_realloc,
        "LocalLock" | "GlobalLock" => lock_block,
        "LocalUnlock" | "GlobalUnlock" => unlock_block,
        "VirtualAlloc" => virtual_alloc,
        "VirtualFree" => virtual_free,
        "VirtualProtect" => virtual_protect,
        "VirtualQuery" => virtual_query,
        "GetWriteWatch" => |_| {
            unsupported_kernel32(
                "GetWriteWatch",
                "page write-watch tracking is not modeled by guest memory",
            )
        },
        // Guest code writes advance Memory's code epoch, so the interpreter drops stale decoded
        // blocks without requiring an explicit cache flush.
        "FlushInstructionCache" => |_| Ok(1),
        "FindResourceA" => find_resource_a,
        "FindResourceW" => find_resource_w,
        "EnumResourceNamesA" => |p| enum_resource_names(p, false),
        "EnumResourceNamesW" => |p| enum_resource_names(p, true),
        "LoadResource" => load_resource,
        "LockResource" => lock_resource,
        "SizeofResource" => sizeof_resource,
        // console / files
        "GetStdHandle" => |p| {
            let index = match p.arg(0) as u32 {
                0xfffffff6 => 0,
                0xfffffff5 => 1,
                0xfffffff4 => 2,
                _ => return Ok(INVALID_HANDLE),
            };
            Ok(p.std_handles[index])
        },
        "SetStdHandle" => set_std_handle,
        "SetErrorMode" => |p| {
            let previous = p.error_mode;
            p.error_mode = p.arg(0) as u32;
            Ok(previous as u64)
        },
        "SetHandleCount" => |p| Ok(p.arg(0) as u32 as u64),
        "SetConsoleCtrlHandler" => set_console_ctrl_handler,
        "FatalAppExitA" => fatal_app_exit_a,
        "WriteFile" => write_file,
        "ReadFile" => read_file,
        "LockFile" => lock_file,
        "UnlockFile" => unlock_file,
        "CreatePipe" => |_| {
            unsupported_kernel32(
                "CreatePipe",
                "anonymous pipe buffering, blocking reads, and inherited-handle lifetime are not modeled",
            )
        },
        "CreateNamedPipeW" => |_| {
            unsupported_kernel32(
                "CreateNamedPipeW",
                "named-pipe server/client IPC is not modeled",
            )
        },
        "ConnectNamedPipe" => |_| {
            unsupported_kernel32(
                "ConnectNamedPipe",
                "named-pipe server/client IPC is not modeled",
            )
        },
        "DisconnectNamedPipe" => |_| {
            unsupported_kernel32(
                "DisconnectNamedPipe",
                "named-pipe server/client IPC is not modeled",
            )
        },
        "CreateProcessW" => |_| {
            unsupported_kernel32(
                "CreateProcessW",
                "the runtime does not load and schedule a second guest process",
            )
        },
        "DeviceIoControl" => |_| {
            unsupported_kernel32(
                "DeviceIoControl",
                "device and filesystem control-code dispatch is not modeled",
            )
        },
        "GetOverlappedResult" => get_overlapped_result,
        "WriteConsoleA" => write_console_a,
        "WriteConsoleW" => write_console_w,
        "GetConsoleMode" => get_console_mode,
        "FlushConsoleInputBuffer" => flush_console_input_buffer,
        "GetConsoleCP" | "GetConsoleOutputCP" => |_| Ok(437),
        "GetConsoleScreenBufferInfo" => get_console_screen_buffer_info,
        // The virtual console has no desktop window.
        "GetConsoleWindow" => |_| Ok(0),
        "GetFileType" => |p| {
            Ok(match p.arg(0) {
                STD_IN | STD_OUT | STD_ERR => 2,
                h if matches!(p.handles.get(&h), Some(Handle::File { .. })) => 1,
                _ => 0,
            })
        },
        "CreateFileA" => |p| create_file(p, false),
        "CreateFileW" => |p| create_file(p, true),
        "CreateFileMappingA" => create_file_mapping,
        "MapViewOfFile" => map_view_of_file,
        "UnmapViewOfFile" => unmap_view_of_file,
        "FindFirstFileA" => |p| find_first_file(p, false),
        "FindFirstFileW" => |p| find_first_file(p, true),
        "FindNextFileA" => |p| find_next_file(p, false),
        "FindNextFileW" => |p| find_next_file(p, true),
        "FindClose" => |p| find_close(p),
        "FindFirstChangeNotificationA" => |p| find_first_change_notification(p, false),
        "FindFirstChangeNotificationW" => |p| find_first_change_notification(p, true),
        "FindNextChangeNotification" => find_next_change_notification,
        "FindCloseChangeNotification" => find_close_change_notification,
        "CloseHandle" => |p| {
            let h = p.arg(0);
            let closed = p.handles.remove(&h).is_some() || matches!(h, STD_IN | STD_OUT | STD_ERR);
            if closed {
                p.extra.file_range_locks.retain(|lock| lock.handle != h);
                p.extra.file_access.remove(&h);
            }
            Ok(closed as u64)
        },
        "GetFileSize" => |p| {
            let h = p.arg(0);
            let length = file_len(p, h);
            trace_file_size(p, h, length, "GetFileSize");
            match length {
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
        "GetFileSizeEx" => |p| {
            let h = p.arg(0);
            let length = file_len(p, h);
            trace_file_size(p, h, length, "GetFileSizeEx");
            match length {
                Some(n) => {
                    let o = p.arg(1);
                    p.wr64(o, n)?;
                    Ok(1)
                }
                None => Ok(0),
            }
        },
        "SetFilePointer" => set_file_pointer,
        "SetFilePointerEx" => set_file_pointer_ex,
        "SetEndOfFile" => set_end_of_file,
        "SetFileTime" => set_file_time,
        "FlushFileBuffers" => flush_file_buffers,
        "CancelIo" => cancel_io,
        "FormatMessageA" => format_message_a,
        "CopyFileA" => |p| {
            let source = p.read_cstr(p.arg(0))?;
            let destination = p.read_cstr(p.arg(1))?;
            let fail_if_exists = p.arg(2) != 0;
            copy_file(p, &source, &destination, fail_if_exists)
        },
        "CopyFileW" => |p| {
            let source = p.read_wstr(p.arg(0))?;
            let destination = p.read_wstr(p.arg(1))?;
            let fail_if_exists = p.arg(2) != 0;
            copy_file(p, &source, &destination, fail_if_exists)
        },
        "MoveFileW" => |p| {
            let source = p.read_wstr(p.arg(0))?;
            let destination = p.read_wstr(p.arg(1))?;
            move_file(p, &source, &destination, 0)
        },
        "MoveFileExW" => |p| {
            let source = p.read_wstr(p.arg(0))?;
            let destination = p.read_wstr(p.arg(1))?;
            let flags = p.arg(2) as u32;
            move_file(p, &source, &destination, flags)
        },
        "DeleteFileA" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            delete_file(p, &s)
        },
        "DeleteFileW" => |p| {
            let s = p.read_wstr(p.arg(0))?;
            delete_file(p, &s)
        },
        "RemoveDirectoryW" => remove_directory_w,
        "GetFileAttributesA" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            Ok(file_attrs(p, &s))
        },
        "GetFileAttributesW" => |p| {
            let s = p.read_wstr(p.arg(0))?;
            let attributes = file_attrs(p, &s);
            if p.trace_imports && attributes == u32::MAX as u64 {
                let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
                p.stderr.extend_from_slice(
                    format!(
                        "[file-miss] api=GetFileAttributesW path={:?} caller={caller:#x}\n",
                        absolute(&s, &p.current_directory)
                    )
                    .as_bytes(),
                );
            }
            Ok(attributes)
        },
        "GetFileAttributesExW" => get_file_attributes_ex_w,
        "SetFileAttributesA" => |p| {
            let s = p.read_cstr(p.arg(0))?;
            let attributes = p.arg(1) as u32;
            set_file_attrs(p, &s, attributes)
        },
        "SetFileAttributesW" => |p| {
            let s = p.read_wstr(p.arg(0))?;
            let attributes = p.arg(1) as u32;
            set_file_attrs(p, &s, attributes)
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
            let (n, b) = (p.arg(0) as u32 as usize, p.arg(1));
            if n >= 9 {
                p.write_wstr(b, "C:\\Temp\\")?;
                Ok(8)
            } else {
                Ok(9)
            }
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
        "GetDiskFreeSpaceExA" | "GetDiskFreeSpaceExW" => get_disk_free_space_ex,
        "GetDiskFreeSpaceA" | "GetDiskFreeSpaceW" => get_disk_free_space,
        "GetDriveTypeA" => get_drive_type_a,
        "GetLogicalDrives" => |_| Ok(1 << 2),
        "GetLogicalDriveStringsA" => get_logical_drive_strings_a,
        "GetVolumeInformationA" => |p| get_volume_information(p, false),
        "GetVolumeInformationW" => |p| get_volume_information(p, true),
        "GetFullPathNameA" => |p| full_path(p, false),
        "GetFullPathNameW" => |p| full_path(p, true),
        "GetLongPathNameW" => get_long_path_name_w,
        "GetModuleFileNameA" => |p| module_file_name(p, false),
        "GetModuleFileNameW" => |p| module_file_name(p, true),
        "GetCommandLineA" => get_command_line_a,
        "GetCommandLineW" => get_command_line_w,
        "GetStartupInfoA" | "GetStartupInfoW" => |p| {
            let a = p.arg(0);
            let size = if p.cpu.mode == CpuMode::X86_32 {
                68
            } else {
                104
            };
            p.write_bytes(a, &vec![0u8; size])?;
            p.wr32(a, size as u32)?;
            Ok(0)
        },
        "GetEnvironmentStringsW" => get_environment_strings_w,
        "GetEnvironmentStrings" | "GetEnvironmentStringsA" => get_environment_strings_a,
        "FreeEnvironmentStringsW" => |p| free_environment_strings(p, true),
        "FreeEnvironmentStringsA" => |p| free_environment_strings(p, false),
        "GetEnvironmentVariableA" => |p| env_var(p, false),
        "GetEnvironmentVariableW" => |p| env_var(p, true),
        "SetEnvironmentVariableA" => |p| set_environment_variable(p, false),
        "SetEnvironmentVariableW" => |p| set_environment_variable(p, true),
        "ExpandEnvironmentStringsA" => |p| expand_environment_strings_a(p),
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
        "FreeLibrary" => free_library,
        "DisableThreadLibraryCalls" => disable_thread_library_calls,
        "GetProcAddress" => get_proc_address,
        // time
        "QueryPerformanceCounter" => |p| {
            let a = p.arg(0);
            let t = query_performance_counter_value(p);
            p.wr64(a, t)?;
            Ok(1)
        },
        "QueryPerformanceFrequency" => |p| {
            let a = p.arg(0);
            p.wr64(a, QUERY_PERFORMANCE_FREQUENCY)?;
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
        "GetDateFormatA" => get_date_format_a,
        "GetTimeFormatA" => get_time_format_a,
        "SystemTimeToFileTime" => system_time_to_file_time,
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
        "GetSystemTimeAdjustment" => get_system_time_adjustment,
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
        "InitializeConditionVariable" => initialize_condition_variable,
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
        "OpenEventW" => |_| {
            unsupported_kernel32(
                "OpenEventW",
                "the named kernel-object namespace is not modeled",
            )
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
        "OpenMutexW" => |_| {
            unsupported_kernel32(
                "OpenMutexW",
                "the named kernel-object namespace is not modeled",
            )
        },
        "ReleaseMutex" => release_mutex,
        "CreateSemaphoreA" | "CreateSemaphoreW" => create_semaphore,
        "CreateSemaphoreExW" => create_semaphore_ex,
        "OpenSemaphoreW" => |_| {
            unsupported_kernel32(
                "OpenSemaphoreW",
                "the named kernel-object namespace is not modeled",
            )
        },
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
        "GetACP" => |_| Ok(VIRTUAL_ANSI_CODE_PAGE as u64),
        "GetOEMCP" => |_| Ok(437),
        "IsValidCodePage" => is_valid_code_page,
        "GetCPInfo" => get_cp_info,
        "GetUserDefaultLCID" | "GetSystemDefaultLCID" | "GetThreadLocale" => |_| Ok(0x409),
        "GetUserDefaultLangID" => |_| Ok(0x0409),
        "IsValidLocale" => is_valid_locale,
        "GetGeoInfoA" => get_geo_info_a,
        "GetUserGeoID" => get_user_geo_id,
        "EnumSystemLocalesA" => enum_system_locales_a,
        "GetUserDefaultLocaleName" | "GetSystemDefaultLocaleName" => get_default_locale_name,
        "GetLocaleInfoA" => |p| get_locale_info(p, false),
        "GetLocaleInfoW" => |p| get_locale_info(p, true),
        "MultiByteToWideChar" => multi_byte_to_wide_char,
        "WideCharToMultiByte" => wide_char_to_multi_byte,
        "lstrlenA" => |p| Ok(p.read_cstr(p.arg(0))?.len() as u64),
        "lstrlenW" => |p| Ok(p.read_wstr(p.arg(0))?.encode_utf16().count() as u64),
        "lstrcatA" => lstrcat_a,
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
        "GetStringTypeW" => get_string_type_w,
        "GetStringTypeExW" => get_string_type_ex_w,
        "GetStringTypeA" => get_string_type_a,
        "LCMapStringW" | "LCMapStringEx" => lc_map_string_w,
        "LCMapStringA" => lc_map_string_a,
        "IsDBCSLeadByte" => is_dbcs_lead_byte,
        "IsDBCSLeadByteEx" => is_dbcs_lead_byte_ex,
        // misc
        "CreateThread" => create_thread,
        "ResumeThread" => resume_thread,
        "GetProcessAffinityMask" => get_process_affinity_mask,
        "SetProcessAffinityMask" => set_process_affinity_mask,
        "GetLogicalProcessorInformationEx" => get_logical_processor_information_ex,
        // A-suffixed file APIs consume the runtime's ANSI byte-string path representation.
        "AreFileApisANSI" => |_| Ok(1),
        "EncodePointer" | "DecodePointer" | "EncodeSystemPointer" | "DecodeSystemPointer" => {
            |p| Ok(p.arg(0))
        }
        "IsWow64Process" => is_wow64_process,
        "GetUserDefaultUILanguage" => |_| Ok(0x409),
        _ => return None,
    })
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn lstrcat_a(p: &mut Process) -> Result<u64, Stop> {
    let destination = p.arg(0);
    let source = p.arg(1);
    let destination_bytes = p
        .mem
        .read_cstr(destination, 1 << 20)
        .map_err(|e| Stop::Crash(format!("read fault {destination:#x}: {e:?}")))?;
    let mut source_bytes = p
        .mem
        .read_cstr(source, 1 << 20)
        .map_err(|e| Stop::Crash(format!("read fault {source:#x}: {e:?}")))?;
    source_bytes.push(0);
    let append_at = destination
        .checked_add(destination_bytes.len() as u64)
        .ok_or_else(|| {
            Stop::Crash(format!(
                "lstrcatA destination address overflow: {destination:#x}"
            ))
        })?;
    p.write_bytes(append_at, &source_bytes)?;
    Ok(destination)
}

fn ticks(p: &Process) -> u64 {
    p.now_ticks()
}

fn get_process_id(p: &mut Process) -> R {
    let handle = p.arg(0);
    if is_current_process_pseudo_handle(handle) {
        return Ok(CURRENT_PROCESS_ID as u64);
    }
    match p.handles.get(&handle) {
        Some(Handle::Process { id, .. }) => Ok(*id as u64),
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn get_exit_code_process(p: &mut Process) -> R {
    let (handle, output) = (p.arg(0), p.arg(1));
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let valid = is_current_process_pseudo_handle(handle)
        || matches!(
            p.handles.get(&handle),
            Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID
        );
    if !valid {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    p.wr32(output, p.exit_code.unwrap_or(259))?;
    Ok(1)
}

fn is_current_process_pseudo_handle(handle: u64) -> bool {
    handle == u64::MAX || handle == u32::MAX as u64
}

fn is_current_thread_pseudo_handle(handle: u64) -> bool {
    handle == u64::MAX - 1 || handle == u32::MAX as u64 - 1
}

fn duplicate_handle(p: &mut Process) -> R {
    let (source_process, source_handle, target_process) = (p.arg(0), p.arg(1), p.arg(2));
    let (target_output, desired_access, inherit, options) =
        (p.arg(3), p.arg(4) as u32, p.arg(5) != 0, p.arg(6) as u32);

    if options & DUPLICATE_CLOSE_SOURCE != 0 {
        return Err(Stop::Crash(
            "untranslated: kernel32!DuplicateHandle with DUPLICATE_CLOSE_SOURCE".into(),
        ));
    }
    if options & !DUPLICATE_SAME_ACCESS != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    if !is_current_process_pseudo_handle(source_process)
        && !matches!(p.handles.get(&source_process), Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID)
    {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if !is_current_process_pseudo_handle(target_process)
        && !matches!(p.handles.get(&target_process), Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID)
    {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if options & DUPLICATE_SAME_ACCESS == 0 {
        return Err(Stop::Crash(format!(
            "untranslated: kernel32!DuplicateHandle access mask {desired_access:#x}"
        )));
    }

    let duplicate = if is_current_process_pseudo_handle(source_handle) {
        Handle::Process {
            id: CURRENT_PROCESS_ID,
            inheritable: inherit,
        }
    } else if is_current_thread_pseudo_handle(source_handle) {
        Handle::Thread {
            id: p.current_thread_id,
            start: 0,
            parameter: 0,
            exit_code: match &p.current_thread_state {
                crate::process::ThreadState::Exited(code) => Some(*code),
                _ => None,
            },
            suspended: false,
            inheritable: inherit,
        }
    } else {
        match p.handles.get(&source_handle) {
            Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID => Handle::Process {
                id: *id,
                inheritable: inherit,
            },
            Some(Handle::Thread {
                id,
                start: 0,
                parameter: 0,
                exit_code,
                suspended: false,
                ..
            }) if *id == p.current_thread_id => Handle::Thread {
                id: *id,
                start: 0,
                parameter: 0,
                exit_code: *exit_code,
                suspended: false,
                inheritable: inherit,
            },
            Some(handle) => {
                return Err(Stop::Crash(format!(
                    "untranslated: kernel32!DuplicateHandle object type {}",
                    handle_type_name(handle)
                )))
            }
            None => {
                p.set_last_error(ERROR_INVALID_HANDLE);
                return Ok(0);
            }
        }
    };

    // Windows permits a null output for compatibility, in which case the new
    // handle is created but its value is intentionally discarded.
    let next_handle = p.next_handle;
    if target_output != 0 {
        p.wrptr(target_output, next_handle)?;
    }
    let created_handle = p.new_handle(duplicate);
    debug_assert_eq!(created_handle, next_handle);
    Ok(1)
}

fn handle_type_name(handle: &Handle) -> &'static str {
    match handle {
        Handle::Std(_) => "standard stream",
        Handle::File { .. } => "file",
        Handle::Mapping { .. } => "file mapping",
        Handle::Find { .. } => "find cursor",
        Handle::Event { .. } => "event",
        Handle::ChangeNotification { .. } => "change notification",
        Handle::Mutex { .. } => "mutex",
        Handle::Semaphore { .. } => "semaphore",
        Handle::Thread { .. } => "thread",
        Handle::Process { .. } => "process",
        Handle::Module(_) => "module",
        Handle::Other => "other",
    }
}

fn check_remote_debugger_present(p: &mut Process) -> R {
    let (process, output) = (p.arg(0), p.arg(1));
    if process != u64::MAX {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.wr32(output, 0)?;
    p.set_last_error(0);
    Ok(1)
}

fn get_process_affinity_mask(p: &mut Process) -> R {
    let (process, process_mask, system_mask) = (p.arg(0), p.arg(1), p.arg(2));
    if !is_current_process_pseudo_handle(process)
        && !matches!(p.handles.get(&process), Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID)
    {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if process_mask == 0 || system_mask == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if p.cpu.mode == CpuMode::X86_32 {
        p.wr32(process_mask, 1)?;
        p.wr32(system_mask, 1)?;
    } else {
        p.wr64(process_mask, 1)?;
        p.wr64(system_mask, 1)?;
    }
    p.set_last_error(0);
    Ok(1)
}

fn set_process_affinity_mask(p: &mut Process) -> R {
    let (process, mask) = (p.arg(0), p.arg(1));
    if !is_current_process_pseudo_handle(process)
        && !matches!(p.handles.get(&process), Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID)
    {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if mask != 1 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.set_last_error(0);
    Ok(1)
}

fn is_wow64_process(p: &mut Process) -> R {
    let (process, output) = (p.arg(0), p.arg(1));
    let current_process_pseudo_handle = if p.cpu.mode == CpuMode::X86_32 {
        process as u32 == u32::MAX
    } else {
        process == u64::MAX
    };
    let current_process_handle = matches!(
        p.handles.get(&process),
        Some(Handle::Process {
            id: CURRENT_PROCESS_ID,
            ..
        })
    );
    if !current_process_pseudo_handle && !current_process_handle {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.wr32(output, (p.cpu.mode == CpuMode::X86_32) as u32)?;
    p.set_last_error(0);
    Ok(1)
}

pub fn query_performance_counter_value(p: &Process) -> u64 {
    ticks(p) / 10
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

fn get_priority_class(p: &mut Process) -> R {
    match p.arg(0) {
        0xffff_ffff | u64::MAX => Ok(p.process_priority_class as u64),
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn set_priority_class(p: &mut Process) -> R {
    match p.arg(0) {
        0xffff_ffff | u64::MAX => {
            let class = p.arg(1) as u32;
            if class == 0 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            p.process_priority_class = class;
            Ok(1)
        }
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn thread_id_from_handle(p: &Process, handle: u64) -> Option<u32> {
    if is_current_thread_pseudo_handle(handle) {
        return Some(p.current_thread_id);
    }
    match p.handles.get(&handle) {
        Some(Handle::Thread { id, .. }) => Some(*id),
        _ => None,
    }
}

fn open_thread(p: &mut Process) -> R {
    let (inherit, id) = (p.arg(1) != 0, p.arg(2) as u32);
    if id == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let state = if id == p.current_thread_id && !p.current_thread_saved {
        Some(&p.current_thread_state)
    } else {
        p.guest_threads.get(&id).map(|thread| &thread.state)
    };
    let Some(state) = state else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let exit_code = match state {
        ThreadState::Exited(code) => Some(*code),
        _ => None,
    };
    let (start, parameter) = p
        .handles
        .values()
        .find_map(|handle| match handle {
            Handle::Thread {
                id: handle_id,
                start,
                parameter,
                ..
            } if *handle_id == id => Some((*start, *parameter)),
            _ => None,
        })
        .unwrap_or((0, 0));
    let suspended = p.thread_suspend_counts.get(&id).copied().unwrap_or(0) != 0;
    Ok(p.new_handle(Handle::Thread {
        id,
        start,
        parameter,
        exit_code,
        suspended,
        inheritable: inherit,
    }))
}

fn suspend_thread(p: &mut Process) -> R {
    let handle = p.arg(0);
    let Some(id) = thread_id_from_handle(p, handle) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(u32::MAX as u64);
    };
    let Some(previous) = p.suspend_thread(id) else {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(u32::MAX as u64);
    };
    for handle in p.handles.values_mut() {
        if let Handle::Thread {
            id: handle_id,
            suspended,
            ..
        } = handle
        {
            if *handle_id == id {
                *suspended = true;
            }
        }
    }
    Ok(previous as u64)
}

fn resume_thread(p: &mut Process) -> R {
    let handle = p.arg(0);
    let Some(id) = thread_id_from_handle(p, handle) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(u32::MAX as u64);
    };
    let Some(previous) = p.resume_thread(id) else {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(u32::MAX as u64);
    };
    let suspended = p.thread_suspend_counts.get(&id).copied().unwrap_or(0) != 0;
    for handle in p.handles.values_mut() {
        if let Handle::Thread {
            id: handle_id,
            suspended: handle_suspended,
            ..
        } = handle
        {
            if *handle_id == id {
                *handle_suspended = suspended;
            }
        }
    }
    Ok(previous as u64)
}

fn terminate_thread(p: &mut Process) -> R {
    let (handle, code) = (p.arg(0), p.arg(1) as u32);
    let Some(id) = thread_id_from_handle(p, handle) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    if id == p.current_thread_id && !p.current_thread_saved {
        return Err(Stop::ThreadExit(code));
    }
    if p.terminate_guest_thread(id, code) {
        Ok(1)
    } else {
        p.set_last_error(ERROR_ACCESS_DENIED);
        Ok(0)
    }
}

fn get_exit_code_thread(p: &mut Process) -> R {
    let (handle, output) = (p.arg(0), p.arg(1));
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let Some(id) = thread_id_from_handle(p, handle) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    let handle_code = match p.handles.get(&handle) {
        Some(Handle::Thread { exit_code, .. }) => *exit_code,
        _ => None,
    };
    let code = handle_code
        .or_else(|| {
            if id == p.current_thread_id && !p.current_thread_saved {
                match p.current_thread_state {
                    ThreadState::Exited(code) => Some(code),
                    _ => None,
                }
            } else {
                match p.guest_threads.get(&id).map(|thread| &thread.state) {
                    Some(ThreadState::Exited(code)) => Some(*code),
                    _ => None,
                }
            }
        })
        .unwrap_or(259);
    p.wr32(output, code)?;
    Ok(1)
}

fn get_thread_priority(p: &mut Process) -> R {
    let Some(thread_id) = thread_id_from_handle(p, p.arg(0)) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(THREAD_PRIORITY_ERROR_RETURN as u64);
    };
    Ok(p.thread_priorities.get(&thread_id).copied().unwrap_or(0) as i64 as u64)
}

fn set_thread_priority(p: &mut Process) -> R {
    let Some(thread_id) = thread_id_from_handle(p, p.arg(0)) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    let priority = p.arg(1) as u32 as i32;
    let standard_priority = matches!(priority, -15 | -2 | -1 | 0 | 1 | 2 | 15);
    let realtime_extension = p.process_priority_class == 0x100 && (-7..=6).contains(&priority);
    if !standard_priority && !realtime_extension {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    p.thread_priorities.insert(thread_id, priority);
    let current_priority = p
        .thread_priorities
        .get(&p.current_thread_id)
        .copied()
        .unwrap_or(0);
    if thread_id != p.current_thread_id && priority > current_priority {
        p.request_thread_yield();
    }
    Ok(1)
}

fn set_thread_execution_state(p: &mut Process) -> R {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    let flags = p.arg(0) as u32;
    if flags != ES_CONTINUOUS {
        return Err(Stop::Crash(format!(
            "untranslated: kernel32!SetThreadExecutionState flags={flags:#010x}; host power requests are not modeled"
        )));
    }

    let state = p
        .extra
        .thread_execution_states
        .entry(p.current_thread_id)
        .or_insert(ES_CONTINUOUS);
    let previous = *state;
    *state = ES_CONTINUOUS;
    Ok(previous as u64)
}

fn set_thread_ideal_processor(p: &mut Process) -> R {
    let (handle, ideal_processor) = (p.arg(0), p.arg(1) as u32);
    let Some(thread_id) = thread_id_from_handle(p, handle) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(u32::MAX as u64);
    };

    let previous = p
        .thread_ideal_processors
        .get(&thread_id)
        .copied()
        .unwrap_or(0);
    let max_processor_sentinel = match p.cpu.mode {
        CpuMode::X86_32 => MAXIMUM_PROCESSORS_X86,
        _ => MAXIMUM_PROCESSORS_X64,
    };
    if ideal_processor == max_processor_sentinel {
        return Ok(previous as u64);
    }
    if ideal_processor >= VIRTUAL_PROCESSOR_COUNT {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(u32::MAX as u64);
    }

    p.thread_ideal_processors.insert(thread_id, ideal_processor);
    Ok(previous as u64)
}

fn raise_exception(p: &mut Process) -> R {
    let (code, flags, arg_count, args_pointer) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        (p.arg(2) as u32).min(15),
        p.arg(3),
    );
    let pointer_size = if p.cpu.mode == CpuMode::X86_32 { 4 } else { 8 };
    let arg_values = (0..arg_count)
        .map(|index| p.rdptr(args_pointer.wrapping_add(index as u64 * pointer_size)))
        .collect::<Result<Vec<_>, _>>()?;
    let caller = p.rdptr(p.cpu.gpr[RSP])?;
    if p.trace_imports {
        let args = arg_values
            .iter()
            .map(|arg| format!("{arg:#x}"))
            .collect::<Vec<_>>()
            .join(", ");
        let stack = (0..72)
            .map(|index| {
                let offset = index * pointer_size;
                let value = p.rdptr(p.cpu.gpr[RSP].wrapping_add(offset)).unwrap_or(0);
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

fn add_vectored_exception_handler(p: &mut Process) -> R {
    add_vectored_handler(p, false)
}

fn add_vectored_continue_handler(p: &mut Process) -> R {
    add_vectored_handler(p, true)
}

fn add_vectored_handler(p: &mut Process, is_continue: bool) -> R {
    let first = p.arg(0) != 0;
    let callback = p.arg(1);
    if callback == 0 {
        return Ok(0);
    }
    let mut handle = p.next_vectored_exception_handle;
    while handle == 0
        || p.vectored_exception_handlers
            .iter()
            .any(|(existing, _)| *existing == handle)
        || p.vectored_continue_handlers
            .iter()
            .any(|(existing, _)| *existing == handle)
    {
        handle = handle.wrapping_add(1).max(0x7000_0000);
    }
    p.next_vectored_exception_handle = handle.wrapping_add(1).max(0x7000_0000);
    let handlers = if is_continue {
        &mut p.vectored_continue_handlers
    } else {
        &mut p.vectored_exception_handlers
    };
    if first {
        handlers.insert(0, (handle, callback));
    } else {
        handlers.push((handle, callback));
    }
    Ok(handle)
}

fn remove_vectored_exception_handler(p: &mut Process) -> R {
    remove_vectored_handler(p, false)
}

fn remove_vectored_continue_handler(p: &mut Process) -> R {
    remove_vectored_handler(p, true)
}

fn remove_vectored_handler(p: &mut Process, is_continue: bool) -> R {
    let handle = p.arg(0);
    let handlers = if is_continue {
        &mut p.vectored_continue_handlers
    } else {
        &mut p.vectored_exception_handlers
    };
    let Some(index) = handlers
        .iter()
        .position(|(registered, _)| *registered == handle)
    else {
        return Ok(0);
    };
    handlers.remove(index);
    Ok(1)
}

fn set_unhandled_exception_filter(p: &mut Process) -> R {
    let previous = p.top_level_exception_filter;
    p.top_level_exception_filter = p.arg(0);
    Ok(previous)
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

pub fn create_thread(p: &mut Process) -> R {
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
    if thread_id_ptr != 0 {
        let current = p.rd32(thread_id_ptr)?;
        p.wr32(thread_id_ptr, current)?;
    }
    let suspended = flags & 4 != 0;
    match p.create_guest_thread(id, start, parameter, suspended) {
        Ok(()) => {}
        Err(Stop::Crash(message)) if message.starts_with("guest allocation failed") => {
            p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return Ok(0);
        }
        Err(stop) => return Err(stop),
    }
    p.next_thread_id = p.next_thread_id.wrapping_add(4).max(0x1008);
    if thread_id_ptr != 0 {
        p.wr32(thread_id_ptr, id)?;
    }
    let handle = p.new_handle(Handle::Thread {
        id,
        start,
        parameter,
        exit_code: None,
        suspended,
        inheritable: false,
    });
    if !suspended {
        p.request_thread_yield();
    }
    Ok(handle)
}

fn sleep_thread(p: &mut Process, alertable: bool) -> R {
    let ms = p.arg(0) as u32;
    if ms == 0 {
        p.request_thread_yield();
        return Ok(0);
    }
    if p.nested != 0 && alertable {
        // No APC queue exists yet. Preserve the requested guest time without trying to
        // schedule away from a synchronous callback that cannot be suspended.
        p.advance_time_100ns((ms as u64).saturating_mul(10_000));
        return Ok(0);
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

fn initialize_condition_variable(p: &mut Process) -> R {
    let condition = p.arg(0);
    if condition == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if p.cpu.mode == CpuMode::X86_32 {
        p.wr32(condition, 0)?;
    } else {
        p.wr64(condition, 0)?;
    }
    p.set_last_error(0);
    Ok(0)
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
    if p.cpu.mode == CpuMode::X86_32 {
        let mut b = [0u8; 36];
        b[0..2].copy_from_slice(&0u16.to_le_bytes()); // PROCESSOR_ARCHITECTURE_INTEL
        b[4..8].copy_from_slice(&4096u32.to_le_bytes());
        b[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
        b[12..16].copy_from_slice(&0x7ffe_ffffu32.to_le_bytes());
        b[16..20].copy_from_slice(&1u32.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[24..28].copy_from_slice(&586u32.to_le_bytes()); // PROCESSOR_INTEL_PENTIUM
        b[28..32].copy_from_slice(&0x10000u32.to_le_bytes());
        b[32..34].copy_from_slice(&6u16.to_le_bytes());
        b[34..36].copy_from_slice(&0x3a09u16.to_le_bytes());
        p.write_bytes(a, &b)?;
    } else {
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
    }
    Ok(0)
}

fn get_logical_processor_information_ex(p: &mut Process) -> R {
    const RELATION_PROCESSOR_PACKAGE: u32 = 3;
    const RECORD_SIZE: u32 = 48;

    let relationship = p.arg(0) as u32;
    let buffer = p.arg(1);
    let returned_length = p.arg(2);
    if returned_length == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if relationship != RELATION_PROCESSOR_PACKAGE {
        if matches!(relationship, 0..=8 | 0xffff) {
            return Err(Stop::Unimplemented {
                dll: "kernel32".into(),
                name: "GetLogicalProcessorInformationEx (relationship)".into(),
            });
        }
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    if buffer == 0 || p.rd32(returned_length)? < RECORD_SIZE {
        p.wr32(returned_length, RECORD_SIZE)?;
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }

    // One virtual package, one group, and one logical processor, consistent with GetSystemInfo.
    // SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX header (8 bytes), followed by a
    // PROCESSOR_RELATIONSHIP with GroupCount=1 and GROUP_AFFINITY mask bit 0 set.
    let mut record = [0u8; RECORD_SIZE as usize];
    record[0..4].copy_from_slice(&RELATION_PROCESSOR_PACKAGE.to_le_bytes());
    record[4..8].copy_from_slice(&RECORD_SIZE.to_le_bytes());
    record[30..32].copy_from_slice(&1u16.to_le_bytes()); // GroupCount
    record[32..40].copy_from_slice(&1u64.to_le_bytes()); // GroupMask[0].Mask
    p.write_bytes(buffer, &record)?;
    p.wr32(returned_length, RECORD_SIZE)?;
    Ok(1)
}

fn global_memory_status(p: &mut Process) -> R {
    let a = p.arg(0);
    if p.cpu.mode == CpuMode::X86_32 {
        let mut b = [0u8; 32];
        for (i, value) in [
            32u32,
            50,
            2 << 30,
            1 << 30,
            u32::MAX,
            2 << 30,
            0x7ffe_0000,
            0x7000_0000,
        ]
        .iter()
        .enumerate()
        {
            b[4 * i..4 * i + 4].copy_from_slice(&value.to_le_bytes());
        }
        p.write_bytes(a, &b)?;
    } else {
        let mut b = [0u8; 64];
        b[0..4].copy_from_slice(&64u32.to_le_bytes());
        b[4..8].copy_from_slice(&50u32.to_le_bytes());
        let total = 8u64 << 30;
        for (i, value) in [
            total,
            total / 2,
            total * 2,
            total,
            1u64 << 47,
            (1u64 << 47) - (1 << 30),
        ]
        .iter()
        .enumerate()
        {
            b[8 + 8 * i..16 + 8 * i].copy_from_slice(&value.to_le_bytes());
        }
        p.write_bytes(a, &b)?;
    }
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

fn get_version_ex(p: &mut Process, wide: bool) -> R {
    let (basic_size, extended_size, csd_bytes) = if wide {
        (276, 284, 256)
    } else {
        (148, 156, 128)
    };
    let a = p.arg(0);
    let sz = p.rd32(a)?;
    if sz != basic_size && sz != extended_size {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    p.wr32(a + 4, 10)?;
    p.wr32(a + 8, 0)?;
    p.wr32(a + 12, 19045)?;
    p.wr32(a + 16, 2)?;
    p.write_bytes(a + 20, &vec![0; csd_bytes])?;
    if sz == extended_size {
        // No service pack is present; this virtual OS is a workstation, not a server SKU.
        p.write_bytes(a + 20 + csd_bytes as u64, &[0, 0, 0, 0, 0, 0, 1, 0])?;
    }
    Ok(1)
}

fn get_product_info(p: &mut Process) -> R {
    let (major, minor, _sp_major, _sp_minor, output) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4),
    );

    // GetProductInfo is available starting with Windows Vista. Windows reports FALSE and clears
    // the output for older target versions. The output pointer is required; invalid guest memory
    // remains a memory fault through Process::wr32 rather than a fabricated success value.
    if output == 0 {
        return Ok(0);
    }
    if major < 6 {
        p.wr32(output, 0)?;
        return Ok(0);
    }

    // Windows Vista's Business SKU is the compatible identifier for a Pro SKU. Windows 7 and
    // later use PRODUCT_PROFESSIONAL (0x30); future target versions use the current SKU mapping.
    let product = if major == 6 && minor == 0 {
        PRODUCT_BUSINESS
    } else {
        PRODUCT_PROFESSIONAL
    };
    p.wr32(output, product)?;
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

fn alloc_heap_block(p: &mut Process, heap: u64, n: u64, zero: bool) -> u64 {
    let mut maps: Vec<(u64, u64)> = Vec::new();
    let allocation = p.heap.alloc_for(heap, n, &mut |base, len| {
        maps.push((base, len));
        true
    });
    for (base, len) in maps {
        if !p.mem.map(base, len, PROT_RW) {
            return 0;
        }
    }
    match allocation {
        Some(address) => {
            if zero {
                let size = p.heap.size_of_for(heap, address).unwrap_or(n) as usize;
                if p.write_bytes(address, &vec![0; size]).is_err() {
                    return 0;
                }
            }
            address
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
    let (heap, fl, n) = (p.arg(0), p.arg(1), p.arg(2));
    if fl & !0x0d != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if !p.heap.has_heap(heap) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    let a = alloc_heap_block(p, heap, n, fl & 0x8 != 0);
    if a == 0 {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    } else {
        p.set_last_error(0);
    }
    Ok(a)
}
fn heap_realloc(p: &mut Process) -> R {
    let (heap, fl, a, n) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if fl & !0x1d != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if !p.heap.has_heap(heap) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if a == 0 {
        let allocation = alloc_heap_block(p, heap, n, fl & 0x8 != 0);
        p.set_last_error(if allocation == 0 {
            ERROR_NOT_ENOUGH_MEMORY
        } else {
            0
        });
        return Ok(allocation);
    }
    let old_size = match p.heap.size_of_for(heap, a) {
        Some(size) => size,
        None => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
    };
    if n <= old_size {
        p.set_last_error(0);
        return Ok(a);
    }
    if fl & 0x10 != 0 {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    let new = alloc_heap_block(p, heap, n, fl & 0x8 != 0);
    if new == 0 {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    let data = p.read_bytes(a, old_size as usize)?;
    p.write_bytes(new, &data)?;
    p.heap.free_for(heap, a);
    p.set_last_error(0);
    Ok(new)
}
fn heap_free(p: &mut Process) -> R {
    let (heap, a) = (p.arg(0), p.arg(2));
    if !p.heap.has_heap(heap) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if a == 0 || p.heap.free_for(heap, a).is_some() {
        p.set_last_error(0);
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        Ok(0)
    }
}

fn heap_create(p: &mut Process) -> R {
    const HEAP_NO_SERIALIZE: u32 = 0x0000_0001;
    const HEAP_GENERATE_EXCEPTIONS: u32 = 0x0000_0004;
    const HEAP_CREATE_ENABLE_EXECUTE: u32 = 0x0004_0000;
    let (flags, initial, maximum) = (p.arg(0) as u32, p.arg(1), p.arg(2));
    let allowed = HEAP_NO_SERIALIZE | HEAP_GENERATE_EXCEPTIONS | HEAP_CREATE_ENABLE_EXECUTE;
    if flags & !allowed != 0 || (maximum != 0 && (maximum < initial || maximum < 0x1000)) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let handle = p.next_handle;
    let mut maps = Vec::new();
    let created = p
        .heap
        .create_private(handle, flags, initial, maximum, &mut |base, len| {
            maps.push((base, len));
            true
        });
    if !created {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    for (base, len) in maps {
        if !p.mem.map(base, len, PROT_RW) {
            let _ = p.heap.destroy_private(handle);
            p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return Ok(0);
        }
    }
    p.next_handle = p.next_handle.wrapping_add(4).max(0x100);
    p.set_last_error(0);
    Ok(handle)
}

fn heap_destroy(p: &mut Process) -> R {
    let handle = p.arg(0);
    if handle == p.heap.process_heap {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    let Some(regions) = p.heap.destroy_private(handle) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    for (base, len) in regions {
        p.mem.unmap(base, len);
    }
    p.set_last_error(0);
    Ok(1)
}
fn local_alloc(p: &mut Process) -> R {
    let (fl, n) = (p.arg(0), p.arg(1));
    if fl & 0x2 != 0 {
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    let allocation = alloc_block(p, n, fl & 0x40 != 0);
    p.set_last_error(if allocation == 0 {
        ERROR_NOT_ENOUGH_MEMORY
    } else {
        0
    });
    Ok(allocation)
}

fn local_realloc(p: &mut Process) -> R {
    let (block, size, flags) = (p.arg(0), p.arg(1), p.arg(2));
    if flags & 0x2 != 0 {
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    if p.heap.size_of(block).is_none() {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    let result = realloc_block(p, block, size, flags);
    if result.as_ref().is_ok_and(|&value| value != 0) {
        p.set_last_error(0);
    } else {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
    }
    result
}

fn global_size(p: &mut Process) -> R {
    let size = p.heap.size_of(p.arg(0));
    p.set_last_error(if size.is_some() {
        0
    } else {
        ERROR_INVALID_HANDLE
    });
    Ok(size.unwrap_or(0))
}

fn heap_size(p: &mut Process) -> R {
    let (heap, block) = (p.arg(0), p.arg(2));
    let size_t_max = if p.cpu.mode == CpuMode::X86_32 {
        u32::MAX as u64
    } else {
        u64::MAX
    };
    if !p.heap.has_heap(heap) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(size_t_max);
    }
    match p.heap.size_of_for(heap, block) {
        Some(size) => {
            p.set_last_error(0);
            Ok(size)
        }
        None => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            Ok(size_t_max)
        }
    }
}

fn heap_validate(p: &mut Process) -> R {
    let (heap, block) = (p.arg(0), p.arg(2));
    if !p.heap.has_heap(heap) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    let valid = block == 0 || p.heap.size_of_for(heap, block).is_some();
    p.set_last_error(if valid { 0 } else { ERROR_INVALID_PARAMETER });
    Ok(valid as u64)
}

fn lock_block(p: &mut Process) -> R {
    let block = p.arg(0);
    if p.heap.size_of(block).is_some() {
        p.set_last_error(0);
        Ok(block)
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

fn unlock_block(p: &mut Process) -> R {
    let block = p.arg(0);
    if p.heap.size_of(block).is_some() {
        p.set_last_error(0);
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
    }
    Ok(0)
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

#[derive(Clone)]
enum ResourceKey {
    Id(u16),
    Name(String),
}

fn resource_module(p: &Process, hmodule: u64) -> Option<(u64, u32, u32)> {
    let module = if hmodule == 0 {
        p.modules.iter().find(|module| !module.is_dll)
    } else {
        p.modules.iter().find(|module| module.base == hmodule)
    }?;
    (module.resource_rva != 0 && module.resource_size >= 16).then_some((
        module.base,
        module.resource_rva,
        module.resource_size,
    ))
}

fn resource_key_a(p: &Process, value: u64) -> Result<ResourceKey, Stop> {
    if value <= 0xffff {
        Ok(ResourceKey::Id(value as u16))
    } else {
        Ok(ResourceKey::Name(p.read_cstr(value)?))
    }
}

fn resource_name(p: &Process, root: u64, size: u32, offset: u32) -> Result<Option<String>, Stop> {
    if offset as u64 + 2 > size as u64 {
        return Ok(None);
    }
    let address = root + offset as u64;
    let length = p.rd16(address)? as u32;
    if length > 1024 || offset as u64 + 2 + length as u64 * 2 > size as u64 {
        return Ok(None);
    }
    let mut units = Vec::with_capacity(length as usize);
    for i in 0..length {
        units.push(p.rd16(address + 2 + i as u64 * 2)?);
    }
    Ok(Some(String::from_utf16_lossy(&units)))
}

fn enum_resource_key_a(p: &Process, value: u64) -> Result<ResourceKey, Stop> {
    if value <= 0xffff {
        return Ok(ResourceKey::Id(value as u16));
    }
    let name = p.read_cstr(value)?;
    Ok(parse_resource_ordinal_name(&name).map_or(ResourceKey::Name(name.clone()), ResourceKey::Id))
}

fn enum_resource_key_w(p: &Process, value: u64) -> Result<ResourceKey, Stop> {
    if value <= 0xffff {
        return Ok(ResourceKey::Id(value as u16));
    }
    let name = p.read_wstr(value)?;
    Ok(parse_resource_ordinal_name(&name).map_or(ResourceKey::Name(name.clone()), ResourceKey::Id))
}

fn parse_resource_ordinal_name(name: &str) -> Option<u16> {
    let digits = name.strip_prefix('#')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u16>().ok()
}

fn resource_keys_match(left: &ResourceKey, right: &ResourceKey) -> bool {
    match (left, right) {
        (ResourceKey::Id(a), ResourceKey::Id(b)) => a == b,
        (ResourceKey::Name(a), ResourceKey::Name(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

fn resource_directory_entries(
    p: &Process,
    root: u64,
    size: u32,
    directory_offset: u32,
) -> Result<Vec<(ResourceKey, u32, bool)>, Stop> {
    if directory_offset as u64 + 16 > size as u64 {
        return Err(Stop::Crash(format!(
            "untranslated: malformed PE resource directory at offset {directory_offset:#x}"
        )));
    }
    let directory = root + directory_offset as u64;
    let count = p.rd16(directory + 12)? as u32 + p.rd16(directory + 14)? as u32;
    let entries_offset = directory_offset as u64 + 16;
    if entries_offset + count as u64 * 8 > size as u64 {
        return Err(Stop::Crash(format!(
            "untranslated: malformed PE resource directory entries at offset {directory_offset:#x}"
        )));
    }

    let mut entries = Vec::with_capacity(count as usize);
    for index in 0..count {
        let entry = root + entries_offset + index as u64 * 8;
        let raw_name = p.rd32(entry)?;
        let key = if raw_name & 0x8000_0000 != 0 {
            let offset = raw_name & 0x7fff_ffff;
            let name = resource_name(p, root, size, offset)?.ok_or_else(|| {
                Stop::Crash(format!(
                    "untranslated: malformed PE resource name at offset {offset:#x}"
                ))
            })?;
            ResourceKey::Name(name)
        } else {
            if raw_name & 0x7fff_0000 != 0 {
                return Err(Stop::Crash(format!(
                    "untranslated: malformed PE resource ID {raw_name:#010x}"
                )));
            }
            ResourceKey::Id(raw_name as u16)
        };
        let target = p.rd32(entry + 4)?;
        let target_offset = target & 0x7fff_ffff;
        if target_offset >= size {
            return Err(Stop::Crash(format!(
                "untranslated: PE resource target {target_offset:#x} is outside its directory"
            )));
        }
        entries.push((key, target_offset, target & 0x8000_0000 != 0));
    }
    Ok(entries)
}

fn enum_resource_names(p: &mut Process, wide: bool) -> R {
    let (hmodule, raw_type, callback, lparam) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if callback == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let resource_type = if wide {
        enum_resource_key_w(p, raw_type)?
    } else {
        enum_resource_key_a(p, raw_type)?
    };
    let Some(module) = (if hmodule == 0 {
        p.modules.iter().find(|module| !module.is_dll)
    } else {
        p.modules.iter().find(|module| module.base == hmodule)
    }) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    if module.resource_rva == 0 || module.resource_size < 16 {
        p.set_last_error(ERROR_RESOURCE_DATA_NOT_FOUND);
        return Ok(0);
    }
    let (root, resource_size) = (
        module.base + module.resource_rva as u64,
        module.resource_size,
    );
    let root_entries = resource_directory_entries(p, root, resource_size, 0)?;
    let Some((_, type_offset, type_is_dir)) = root_entries
        .into_iter()
        .find(|(key, _, _)| resource_keys_match(key, &resource_type))
    else {
        p.set_last_error(ERROR_RESOURCE_TYPE_NOT_FOUND);
        return Ok(0);
    };
    if !type_is_dir {
        p.set_last_error(ERROR_RESOURCE_TYPE_NOT_FOUND);
        return Ok(0);
    }

    let names = resource_directory_entries(p, root, resource_size, type_offset)?;
    if names.is_empty() {
        p.set_last_error(ERROR_RESOURCE_NAME_NOT_FOUND);
        return Ok(0);
    }
    for (name, _, _) in names {
        let mut temporary_name = 0;
        let name_arg = match name {
            ResourceKey::Id(id) => id as u64,
            ResourceKey::Name(name) => {
                let bytes = if wide {
                    let mut encoded = name
                        .encode_utf16()
                        .flat_map(u16::to_le_bytes)
                        .collect::<Vec<_>>();
                    encoded.extend_from_slice(&[0, 0]);
                    encoded
                } else {
                    let mut encoded = cp_encode(1252, &name.encode_utf16().collect::<Vec<_>>());
                    encoded.push(0);
                    encoded
                };
                temporary_name = alloc_block(p, bytes.len() as u64, false);
                if temporary_name == 0 {
                    p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
                    return Ok(0);
                }
                if let Err(error) = p.write_bytes(temporary_name, &bytes) {
                    free_block(p, temporary_name);
                    return Err(error);
                }
                temporary_name
            }
        };
        let result = p.call_guest(callback, &[hmodule, raw_type, name_arg, lparam]);
        if temporary_name != 0 {
            free_block(p, temporary_name);
        }
        let should_continue = result? as u32 as i32 != 0;
        if !should_continue {
            p.set_last_error(ERROR_RESOURCE_ENUM_USER_STOP);
            return Ok(0);
        }
    }
    Ok(1)
}

/// Return the target offset and directory bit for a resource-directory child.
fn resource_child(
    p: &Process,
    root: u64,
    size: u32,
    directory_offset: u32,
    key: Option<&ResourceKey>,
) -> Result<Option<(u32, bool)>, Stop> {
    if directory_offset as u64 + 16 > size as u64 {
        return Ok(None);
    }
    let directory = root + directory_offset as u64;
    let count = p.rd16(directory + 12)? as u32 + p.rd16(directory + 14)? as u32;
    let entries_offset = directory_offset as u64 + 16;
    if entries_offset + count as u64 * 8 > size as u64 {
        return Ok(None);
    }
    for index in 0..count {
        let entry = root + entries_offset + index as u64 * 8;
        let raw_name = p.rd32(entry)?;
        let matches = match key {
            None => true,
            Some(ResourceKey::Id(id)) => raw_name & 0x8000_0000 == 0 && raw_name as u16 == *id,
            Some(ResourceKey::Name(name)) if raw_name & 0x8000_0000 != 0 => {
                resource_name(p, root, size, raw_name & 0x7fff_ffff)?
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
            }
            Some(ResourceKey::Name(_)) => false,
        };
        if matches {
            let target = p.rd32(entry + 4)?;
            let offset = target & 0x7fff_ffff;
            if offset as u64 >= size as u64 {
                return Ok(None);
            }
            return Ok(Some((offset, target & 0x8000_0000 != 0)));
        }
    }
    Ok(None)
}

fn find_resource_a(p: &mut Process) -> R {
    let hmodule = p.arg(0);
    let resource_type = resource_key_a(p, p.arg(2))?;
    let resource_name = resource_key_a(p, p.arg(1))?;
    find_resource_entry(p, hmodule, &resource_type, &resource_name)
}

fn find_resource_w(p: &mut Process) -> R {
    let hmodule = p.arg(0);
    let resource_type = resource_key_w(p, p.arg(2))?;
    let resource_name = resource_key_w(p, p.arg(1))?;
    find_resource_entry(p, hmodule, &resource_type, &resource_name)
}

fn resource_key_w(p: &Process, value: u64) -> Result<ResourceKey, Stop> {
    if value <= 0xffff {
        Ok(ResourceKey::Id(value as u16))
    } else {
        Ok(ResourceKey::Name(p.read_wstr(value)?))
    }
}

fn find_resource_entry(
    p: &Process,
    hmodule: u64,
    resource_type: &ResourceKey,
    resource_name: &ResourceKey,
) -> Result<u64, Stop> {
    let Some((base, resource_rva, resource_size)) = resource_module(p, hmodule) else {
        return Ok(0);
    };
    let root = base + resource_rva as u64;
    let Some((type_offset, type_is_dir)) =
        resource_child(p, root, resource_size, 0, Some(resource_type))?
    else {
        return Ok(0);
    };
    if !type_is_dir {
        return Ok(0);
    }
    let Some((mut data_offset, mut is_dir)) =
        resource_child(p, root, resource_size, type_offset, Some(resource_name))?
    else {
        return Ok(0);
    };
    // A named resource normally has one more directory level for language selection.
    for _ in 0..3 {
        if !is_dir {
            break;
        }
        let Some((next_offset, next_is_dir)) =
            resource_child(p, root, resource_size, data_offset, None)?
        else {
            return Ok(0);
        };
        data_offset = next_offset;
        is_dir = next_is_dir;
    }
    if is_dir || data_offset as u64 + 16 > resource_size as u64 {
        return Ok(0);
    }
    Ok(root + data_offset as u64)
}

pub(crate) fn resource_payload_by_id(
    p: &Process,
    hmodule: u64,
    resource_type: u16,
    resource_name: u16,
) -> Result<Option<(u64, u32)>, Stop> {
    let entry = find_resource_entry(
        p,
        hmodule,
        &ResourceKey::Id(resource_type),
        &ResourceKey::Id(resource_name),
    )?;
    if entry == 0 {
        return Ok(None);
    }
    resource_data_info(p, entry)
}

fn resource_data_info(p: &Process, resource: u64) -> Result<Option<(u64, u32)>, Stop> {
    for module in &p.modules {
        if module.resource_rva == 0 || module.resource_size < 16 {
            continue;
        }
        let root = module.base + module.resource_rva as u64;
        let end = root + module.resource_size as u64;
        if resource >= root && resource.saturating_add(16) <= end {
            let rva = p.rd32(resource)?;
            let size = p.rd32(resource + 4)?;
            if rva as u64 + size as u64 <= module.size {
                return Ok(Some((module.base + rva as u64, size)));
            }
        }
    }
    Ok(None)
}

fn load_resource(p: &mut Process) -> R {
    let resource = p.arg(1);
    Ok(if resource_data_info(p, resource)?.is_some() {
        resource
    } else {
        0
    })
}

fn lock_resource(p: &mut Process) -> R {
    Ok(resource_data_info(p, p.arg(0))?
        .map(|(address, _)| address)
        .unwrap_or(0))
}

fn sizeof_resource(p: &mut Process) -> R {
    Ok(resource_data_info(p, p.arg(1))?
        .map(|(_, size)| size as u64)
        .unwrap_or(0))
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
        h => {
            let (path, at) = match p.handles.get(&h) {
                Some(Handle::File {
                    path,
                    pos,
                    write: true,
                    ..
                }) => (path.clone(), *pos),
                _ => return None,
            };
            let file = p.vfs.get_mut(&path)?;
            let old_len = file.len();
            if file.len() < at + data.len() {
                file.resize(at + data.len(), 0);
            }
            file[at..at + data.len()].copy_from_slice(data);
            let new_len = file.len();
            if let Some(Handle::File { pos, .. }) = p.handles.get_mut(&h) {
                *pos = at.saturating_add(data.len());
            }
            if !data.is_empty() {
                let mut changed = FILE_NOTIFY_CHANGE_LAST_WRITE;
                if old_len != new_len {
                    changed |= FILE_NOTIFY_CHANGE_SIZE;
                }
                p.notify_vfs_change(&path, changed);
            }
            Some(data.len())
        }
    }
}

fn write_file(p: &mut Process) -> R {
    let (h, buf, n, written) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    let cache_write = if p.trace_imports {
        p.handles.get(&h).and_then(|handle| match handle {
            Handle::File { path, pos, .. } if path.ends_with("caspartcache.package") => {
                Some((path.clone(), *pos))
            }
            _ => None,
        })
    } else {
        None
    };
    if let Some(Handle::File {
        path,
        pos,
        write: true,
        ..
    }) = p.handles.get(&h)
    {
        if file_range_locked_for_other_handle(p, h, path, *pos as u64, n as u64) {
            p.set_last_error(ERROR_LOCK_VIOLATION);
            if written != 0 {
                p.wr32(written, 0)?;
            }
            return Ok(0);
        }
        if p.vfs.is_readonly(path) {
            p.set_last_error(ERROR_ACCESS_DENIED);
            return Ok(0);
        }
    }
    let data = p.read_bytes(buf, n)?;
    match emit(p, h, &data) {
        Some(k) => {
            if let Some((path, at)) = cache_write {
                let bytes = data
                    .iter()
                    .take(128)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                p.stderr.extend_from_slice(
                    format!(
                        "[cache-file-write] path={path:?} offset={at:#x} requested={n:#x} written={k:#x} bytes={bytes}\n"
                    )
                    .as_bytes(),
                );
            }
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

fn file_range_locked_for_other_handle(
    p: &Process,
    handle: u64,
    path: &str,
    offset: u64,
    length: u64,
) -> bool {
    if length == 0 {
        return false;
    }
    let Some(end) = offset.checked_add(length) else {
        return true;
    };
    p.extra.file_range_locks.iter().any(|lock| {
        if lock.handle == handle || lock.path != path {
            return false;
        }
        let lock_end = lock.offset.saturating_add(lock.length);
        offset < lock_end && lock.offset < end
    })
}

fn byte_range_args(p: &Process) -> (u64, u64) {
    let offset = ((p.arg(2) as u32 as u64) << 32) | p.arg(1) as u32 as u64;
    let length = ((p.arg(4) as u32 as u64) << 32) | p.arg(3) as u32 as u64;
    (offset, length)
}

fn lock_file(p: &mut Process) -> R {
    let handle = p.arg(0);
    let (offset, length) = byte_range_args(p);
    let Some(path) = p.handles.get(&handle).and_then(|object| match object {
        Handle::File { path, .. } => Some(path.clone()),
        _ => None,
    }) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    if p.extra
        .file_access
        .get(&handle)
        .is_some_and(|access| access & (GENERIC_READ | GENERIC_WRITE | GENERIC_ALL) == 0)
    {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    // Windows treats a zero-byte LockFile request as the byte at the requested offset.
    let length = length.max(1);
    let Some(end) = offset.checked_add(length) else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    if p.extra.file_range_locks.iter().any(|lock| {
        if lock.path != path {
            return false;
        }
        let lock_end = lock.offset.saturating_add(lock.length);
        offset < lock_end && lock.offset < end
    }) {
        p.set_last_error(ERROR_LOCK_VIOLATION);
        return Ok(0);
    }
    p.extra.file_range_locks.push(FileRangeLock {
        handle,
        path,
        offset,
        length,
    });
    Ok(1)
}

fn unlock_file(p: &mut Process) -> R {
    let handle = p.arg(0);
    let (offset, length) = byte_range_args(p);
    let Some(path) = p.handles.get(&handle).and_then(|object| match object {
        Handle::File { path, .. } => Some(path.clone()),
        _ => None,
    }) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    if p.extra
        .file_access
        .get(&handle)
        .is_some_and(|access| access & (GENERIC_READ | GENERIC_WRITE | GENERIC_ALL) == 0)
    {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let length = length.max(1);
    let Some(end) = offset.checked_add(length) else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let Some(index) = p.extra.file_range_locks.iter().position(|lock| {
        lock.handle == handle && lock.path == path && lock.offset == offset && lock.length == length
    }) else {
        p.set_last_error(ERROR_NOT_LOCKED);
        return Ok(0);
    };
    debug_assert_eq!(end, offset + length);
    p.extra.file_range_locks.remove(index);
    Ok(1)
}

fn read_file(p: &mut Process) -> R {
    let (h, buf, n, read) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    let overlapped = p.arg(4);
    let data: Vec<u8> = if h == STD_IN {
        let k = n.min(p.stdin.len());
        p.stdin.drain(..k).collect()
    } else if let Some(Handle::File {
        path,
        pos,
        overlapped: handle_overlapped,
        ..
    }) = p.handles.get(&h)
    {
        let path = path.clone();
        let handle_overlapped = *handle_overlapped;
        if handle_overlapped && overlapped == 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            if read != 0 {
                p.wr32(read, 0)?;
            }
            return Ok(0);
        }
        let at = if overlapped != 0 {
            let (offset_at, offset_high_at) = if p.cpu.mode == CpuMode::X86_32 {
                (overlapped + 8, overlapped + 12)
            } else {
                (overlapped + 16, overlapped + 20)
            };
            ((p.rd32(offset_high_at)? as u64) << 32) | p.rd32(offset_at)? as u64
        } else {
            *pos as u64
        };
        let readable = p
            .vfs
            .file_len(&path)
            .unwrap_or(0)
            .saturating_sub(at)
            .min(n as u64);
        if file_range_locked_for_other_handle(p, h, &path, at, readable) {
            p.set_last_error(ERROR_LOCK_VIOLATION);
            if read != 0 {
                p.wr32(read, 0)?;
            }
            return Ok(0);
        }
        let read_end = buf.saturating_add(n as u64);
        if p.trace_gta_eventvol
            && ((buf <= 0x31b0_85dc && read_end > 0x31b0_7318)
                || path.ends_with("models\\particle.txd"))
        {
            let overlapped_offset = (overlapped != 0).then(|| {
                let (offset_at, high_at) = if p.cpu.mode == CpuMode::X86_32 {
                    (overlapped + 8, overlapped + 12)
                } else {
                    (overlapped + 16, overlapped + 20)
                };
                ((p.mem.read_u32(high_at).unwrap_or(0) as u64) << 32)
                    | p.mem.read_u32(offset_at).unwrap_or(0) as u64
            });
            let overlapped_words = (0..if p.cpu.mode == CpuMode::X86_32 { 5 } else { 7 })
                .map(|i| p.mem.read_u32(overlapped + i * 4).unwrap_or(0))
                .map(|word| format!("{word:#x}"))
                .collect::<Vec<_>>();
            let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
            p.stderr.extend_from_slice(
                format!(
                    "[gta-file-read] path={path:?} currentOffset={at:#x} requested={n:#x} guestBuffer={buf:#x} overlapped={overlapped:#x} overlappedWords={overlapped_words:?} overlappedOffset={overlapped_offset:?} caller={caller:#x}\n"
                )
                .as_bytes(),
            );
        }
        let d = match p.vfs.read_range(&path, at, n) {
            Ok(data) => data,
            // Preserve the existing behavior if a file is removed after it was opened.
            Err(AssetReadError::NotFound) => Vec::new(),
            Err(AssetReadError::TooLarge) => {
                p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
                if read != 0 {
                    p.wr32(read, 0)?;
                }
                return Ok(0);
            }
            Err(AssetReadError::Unavailable | AssetReadError::Failed) => {
                p.set_last_error(ERROR_READ_FAULT);
                if read != 0 {
                    p.wr32(read, 0)?;
                }
                return Ok(0);
            }
        };
        if p.trace_imports && path.ends_with("caspartcache.package") {
            let bytes = d
                .iter()
                .take(128)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            p.stderr.extend_from_slice(
                format!(
                    "[cache-file-read] path={path:?} offset={at:#x} requested={n:#x} returned={:#x} bytes={bytes}\n",
                    d.len()
                )
                .as_bytes(),
            );
        }
        if p.trace_gta_eventvol && path.ends_with("models\\particle.txd") {
            let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
            p.stderr.extend_from_slice(
                format!(
                    "[gta-file-read-result] path={path:?} offset={at:#x} requested={n:#x} returned={:#x} fileSize={:?} caller={caller:#x}\n",
                    d.len(),
                    p.vfs.file_len(&path)
                )
                .as_bytes(),
            );
        }
        let next = at.saturating_add(d.len() as u64);
        if overlapped != 0 {
            if handle_overlapped {
                if p.cpu.mode == CpuMode::X86_32 {
                    p.wr32(overlapped, 0)?;
                    p.wr32(overlapped + 4, d.len() as u32)?;
                } else {
                    p.wr64(overlapped, 0)?;
                    p.wr64(overlapped + 8, d.len() as u64)?;
                }
                let event_at = if p.cpu.mode == CpuMode::X86_32 {
                    overlapped + 16
                } else {
                    overlapped + 24
                };
                let event = p.rdptr(event_at)?;
                if let Some(Handle::Event { signaled, .. }) = p.handles.get_mut(&event) {
                    *signaled = true;
                }
            } else {
                let (offset_at, offset_high_at) = if p.cpu.mode == CpuMode::X86_32 {
                    (overlapped + 8, overlapped + 12)
                } else {
                    (overlapped + 16, overlapped + 20)
                };
                p.wr32(offset_at, next as u32)?;
                p.wr32(offset_high_at, (next >> 32) as u32)?;
                if let Some(Handle::File { pos, .. }) = p.handles.get_mut(&h) {
                    *pos = next.min(usize::MAX as u64) as usize;
                }
            }
        } else if let Some(Handle::File { pos, .. }) = p.handles.get_mut(&h) {
            *pos = next.min(usize::MAX as u64) as usize;
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

fn get_overlapped_result(p: &mut Process) -> R {
    let (handle, overlapped, transferred) = (p.arg(0), p.arg(1), p.arg(2));
    if !p.handles.contains_key(&handle) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if overlapped == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let (internal, completed) = if p.cpu.mode == CpuMode::X86_32 {
        (p.rd32(overlapped)?, p.rd32(overlapped + 4)?)
    } else {
        (p.rd64(overlapped)? as u32, p.rd64(overlapped + 8)? as u32)
    };
    if internal == 0x103 {
        // The runtime currently completes file I/O synchronously, so it cannot make forward
        // progress while waiting for a pending overlapped operation.
        p.set_last_error(996); // ERROR_IO_INCOMPLETE
        return Ok(0);
    }
    if internal != 0 {
        p.set_last_error(internal);
        return Ok(0);
    }
    if transferred != 0 {
        p.wr32(transferred, completed)?;
    }
    Ok(1)
}

fn write_console_a(p: &mut Process) -> R {
    let (h, buf, n, w) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    if !matches!(h, STD_OUT | STD_ERR) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if w == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let data = p.read_bytes(buf, n)?;
    emit(p, h, &data);
    p.wr32(w, n as u32)?;
    p.set_last_error(0);
    Ok(1)
}

fn write_console_w(p: &mut Process) -> R {
    let (h, buf, n, w) = (p.arg(0), p.arg(1), p.arg(2) as usize, p.arg(3));
    if !matches!(h, STD_OUT | STD_ERR) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if w == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let raw = p.read_bytes(buf, n * 2)?;
    let u: Vec<u16> = raw
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&u);
    emit(p, h, s.as_bytes());
    p.wr32(w, n as u32)?;
    p.set_last_error(0);
    Ok(1)
}

fn get_console_mode(p: &mut Process) -> R {
    let (handle, output) = (p.arg(0), p.arg(1));
    let mode = match handle {
        STD_IN => 0x7,            // processed, line, and echo input
        STD_OUT | STD_ERR => 0x3, // processed output and line wrapping
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(0);
        }
    };
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.wr32(output, mode)?;
    p.set_last_error(0);
    Ok(1)
}

fn flush_console_input_buffer(p: &mut Process) -> R {
    if p.arg(0) != STD_IN {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    p.stdin.clear();
    p.set_last_error(0);
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
    let normalized_path = p.vfs.normalize_path(&path);
    if p.trace_gta_eventvol
        && matches!(
            normalized_path.as_str(),
            "audio\\sfx\\genrl" | "models\\particle.txd"
        )
    {
        let flags = p.arg(5) as u32;
        let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
        p.stderr.extend_from_slice(
            format!(
                "[gta-file-open] path={normalized_path:?} flags={flags:#x} overlapped={} caller={caller:#x}\n",
                flags & 0x4000_0000 != 0
            )
            .as_bytes(),
        );
    }
    let write = access & 0x4000_0000 != 0 || access & 0x1_0000 != 0 || access & 2 != 0;
    let exists = p.vfs.exists(&path);
    if p.trace_imports
        && normalized_path
            .to_ascii_lowercase()
            .ends_with("caspartcache.package")
    {
        let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
        p.stderr.extend_from_slice(
            format!(
                "[file-open-attempt] api={} path={normalized_path:?} access={access:#x} share={_share:#x} disposition={disp} exists={exists} caller={caller:#x}\n",
                if wide { "CreateFileW" } else { "CreateFileA" }
            )
            .as_bytes(),
        );
    }
    if p.trace_imports && !exists && matches!(disp, 3 | 5) {
        let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
        p.stderr.extend_from_slice(
            format!(
                "[file-miss] api={} path={normalized_path:?} disposition={disp} caller={caller:#x}\n",
                if wide { "CreateFileW" } else { "CreateFileA" }
            )
            .as_bytes(),
        );
    }
    if exists && write && p.vfs.is_readonly(&path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(INVALID_HANDLE);
    }
    let previous_len = p.vfs.file_len(&path);
    let created = !exists && matches!(disp, 1 | 2 | 4);
    let truncated = exists && matches!(disp, 2 | 5);
    match disp {
        1 if exists => {
            p.set_last_error(ERROR_FILE_EXISTS);
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
    if created {
        p.notify_vfs_change(
            &normalized_path,
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_CREATION,
        );
    } else if truncated {
        let mut changed = FILE_NOTIFY_CHANGE_LAST_WRITE;
        if previous_len.unwrap_or(0) != 0 {
            changed |= FILE_NOTIFY_CHANGE_SIZE;
        }
        p.notify_vfs_change(&normalized_path, changed);
    }
    p.set_last_error(if exists && matches!(disp, 2 | 4) {
        ERROR_ALREADY_EXISTS
    } else {
        0
    });
    let handle = p.new_handle(Handle::File {
        path: normalized_path,
        pos: 0,
        write,
        overlapped: p.arg(5) as u32 & 0x4000_0000 != 0,
    });
    p.extra.file_access.insert(handle, access as u32);
    Ok(handle)
}

fn create_file_mapping(p: &mut Process) -> R {
    let (file, protection, size_high, size_low, name_ptr) = (
        p.arg(0),
        p.arg(2),
        p.arg(3) as u32,
        p.arg(4) as u32,
        p.arg(5),
    );
    let requested_size = ((size_high as u64) << 32) | size_low as u64;
    let file_path = if file as u32 == u32::MAX {
        None
    } else {
        match p.handles.get(&file) {
            Some(Handle::File { path, .. }) => Some(path.clone()),
            _ => {
                p.set_last_error(ERROR_INVALID_HANDLE);
                return Ok(0);
            }
        }
    };
    if name_ptr != 0 {
        // Named mappings are currently independent volatile mappings.
        let _ = p.read_cstr(name_ptr)?;
    }
    let size = if requested_size == 0 {
        file_path
            .as_ref()
            .and_then(|path| p.vfs.file_len(path))
            .unwrap_or(0)
    } else {
        requested_size
    };
    if size == 0 || usize::try_from(size).is_err() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let handle = p.new_handle(Handle::Mapping {
        size,
        file_path,
        protection: prot_from_win(protection),
    });
    p.set_last_error(0);
    Ok(handle)
}

fn map_view_of_file(p: &mut Process) -> R {
    let (mapping, access, offset_high, offset_low, requested_len) = (
        p.arg(0),
        p.arg(1),
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4),
    );
    let (size, file_path, _protection) = match p.handles.get(&mapping) {
        Some(Handle::Mapping {
            size,
            file_path,
            protection,
        }) => (*size, file_path.clone(), *protection),
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(0);
        }
    };
    let offset = ((offset_high as u64) << 32) | offset_low as u64;
    if offset >= size {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let len = if requested_len == 0 {
        size - offset
    } else {
        requested_len
    };
    if len == 0 || len > size - offset || usize::try_from(len).is_err() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if p.trace_gta_eventvol {
        let path = file_path.as_deref().unwrap_or("<pagefile>");
        p.stderr.extend_from_slice(
            format!(
                "[gta-map-view] path={path:?} fileSize={size:#x} offset={offset:#x} len={len:#x}\n"
            )
            .as_bytes(),
        );
    }
    let guest_protection = if access & 0x2 != 0 || access & 0x1 != 0 {
        PROT_RW
    } else if access & 0x20 != 0 {
        PROT_R | PROT_X
    } else {
        PROT_R
    };
    let contents = if let Some(path) = file_path.as_deref() {
        match p.vfs.read_range(path, offset, len as usize) {
            Ok(data) => Some(data),
            Err(AssetReadError::NotFound) => None,
            Err(AssetReadError::TooLarge) => {
                p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
                return Ok(0);
            }
            Err(AssetReadError::Unavailable | AssetReadError::Failed) => {
                p.set_last_error(ERROR_READ_FAULT);
                return Ok(0);
            }
        }
    } else {
        None
    };
    let address = p.alloc_virtual(len, guest_protection);
    if p.trace_gta_eventvol {
        p.stderr.extend_from_slice(
            format!("[gta-map-view] mapped address={address:#x} len={len:#x}\n").as_bytes(),
        );
    }
    if let Some(contents) = contents {
        p.mem
            .write_force(address, &contents)
            .map_err(|e| Stop::Crash(format!("mapped file initialization failed: {e:?}")))?;
    }
    p.set_last_error(0);
    Ok(address)
}

fn unmap_view_of_file(p: &mut Process) -> R {
    let address = p.arg(0);
    let Some((len, _)) = p.vmem.remove(&address) else {
        return Ok(0);
    };
    p.mem.unmap(address, len);
    Ok(1)
}

fn is_bad_memory_range(p: &Process, address: u64, size: u64, permission: u8) -> bool {
    if size == 0 {
        return false;
    }
    let Some(last) = address.checked_add(size - 1) else {
        return true;
    };
    let mut page = address >> 12;
    let last_page = last >> 12;
    loop {
        if p.mem
            .prot_of(page << 12)
            .is_none_or(|prot| prot & permission == 0)
        {
            return true;
        }
        if page == last_page {
            return false;
        }
        let Some(next) = page.checked_add(1) else {
            return true;
        };
        page = next;
    }
}

fn file_len(p: &Process, h: u64) -> Option<u64> {
    match p.handles.get(&h) {
        Some(Handle::File { path, .. }) => p.vfs.file_len(path),
        _ => None,
    }
}

fn trace_file_size(p: &mut Process, h: u64, length: Option<u64>, api: &str) {
    let Some(Handle::File { path, pos, .. }) = p.handles.get(&h) else {
        return;
    };
    if !p.trace_gta_eventvol || !path.ends_with("models\\particle.txd") {
        return;
    }
    let (path, pos) = (path.clone(), *pos);
    let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
    p.stderr.extend_from_slice(
        format!("[gta-file-size] api={api} path={path:?} size={length:?} position={pos:#x} caller={caller:#x}\n")
            .as_bytes(),
    );
}

fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let pattern = if pattern == "*.*" { "*" } else { &pattern };
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let (mut pi, mut ni, mut star, mut retry) = (0, 0, None, 0);
    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            retry = ni;
        } else if let Some(star_at) = star {
            retry += 1;
            ni = retry;
            pi = star_at + 1;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn write_find_data(p: &mut Process, out: u64, entry: &FindEntry, wide: bool) -> Result<(), Stop> {
    let mut data = vec![0u8; if wide { 592 } else { 320 }];
    data[0..4].copy_from_slice(&entry.attributes.to_le_bytes());
    data[32..36].copy_from_slice(&((entry.size >> 32) as u32).to_le_bytes());
    data[36..40].copy_from_slice(&(entry.size as u32).to_le_bytes());
    if wide {
        for (i, unit) in entry.name.encode_utf16().take(259).enumerate() {
            data[44 + i * 2..46 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
    } else {
        let name = entry.name.as_bytes();
        let n = name.len().min(259);
        data[44..44 + n].copy_from_slice(&name[..n]);
    }
    p.write_bytes(out, &data)
}

fn find_first_file(p: &mut Process, wide: bool) -> R {
    let (pattern, out) = (p.arg(0), p.arg(1));
    if pattern == 0 || out == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(INVALID_HANDLE);
    }
    let raw = if wide {
        p.read_wstr(pattern)?
    } else {
        p.read_cstr(pattern)?
    };
    let pattern = p.vfs.normalize_path(&absolute(&raw, &p.current_directory));
    let (directory, leaf) = pattern.rsplit_once('\\').unwrap_or(("", pattern.as_str()));
    let entries: Vec<FindEntry> = p
        .vfs
        .directory_entries(directory)
        .into_iter()
        .filter(|(name, _, _)| wildcard_matches(leaf, name))
        .map(|(name, is_directory, size)| FindEntry {
            name,
            size,
            attributes: if is_directory { 0x10 } else { 0x80 },
        })
        .collect();
    let Some(first) = entries.first().cloned() else {
        if p.trace_imports {
            let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
            p.stderr.extend_from_slice(
                format!(
                    "[file-miss] api={} pattern={pattern:?} caller={caller:#x}\n",
                    if wide {
                        "FindFirstFileW"
                    } else {
                        "FindFirstFileA"
                    }
                )
                .as_bytes(),
            );
        }
        p.set_last_error(ERROR_FILE_NOT_FOUND);
        return Ok(INVALID_HANDLE);
    };
    write_find_data(p, out, &first, wide)?;
    p.set_last_error(0);
    let handle = p.new_handle(Handle::Find {
        entries,
        next_index: 1,
    });
    Ok(handle)
}

fn find_next_file(p: &mut Process, wide: bool) -> R {
    let (handle, out) = (p.arg(0), p.arg(1));
    let entry = match p.handles.get_mut(&handle) {
        Some(Handle::Find {
            entries,
            next_index,
        }) if *next_index < entries.len() => {
            let entry = entries[*next_index].clone();
            *next_index += 1;
            entry
        }
        _ => {
            p.set_last_error(if p.handles.contains_key(&handle) {
                18 // ERROR_NO_MORE_FILES
            } else {
                ERROR_INVALID_HANDLE
            });
            return Ok(0);
        }
    };
    write_find_data(p, out, &entry, wide)?;
    p.set_last_error(0);
    Ok(1)
}

fn find_close(p: &mut Process) -> R {
    let handle = p.arg(0);
    if matches!(p.handles.get(&handle), Some(Handle::Find { .. })) {
        p.handles.remove(&handle);
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x0001;
const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x0002;
const FILE_NOTIFY_CHANGE_ATTRIBUTES: u32 = 0x0004;
const FILE_NOTIFY_CHANGE_SIZE: u32 = 0x0008;
const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x0010;
const FILE_NOTIFY_CHANGE_LAST_ACCESS: u32 = 0x0020;
const FILE_NOTIFY_CHANGE_CREATION: u32 = 0x0040;
const FILE_NOTIFY_CHANGE_SECURITY: u32 = 0x0100;
const FILE_NOTIFY_CHANGE_SUPPORTED: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_ATTRIBUTES
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_LAST_ACCESS
    | FILE_NOTIFY_CHANGE_CREATION
    | FILE_NOTIFY_CHANGE_SECURITY;

fn find_first_change_notification(p: &mut Process, wide: bool) -> R {
    let (path_ptr, subtree, filter) = (p.arg(0), p.arg(1) != 0, p.arg(2) as u32);
    if path_ptr == 0 || filter == 0 || filter & !FILE_NOTIFY_CHANGE_SUPPORTED != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(INVALID_HANDLE);
    }
    if filter & (FILE_NOTIFY_CHANGE_SECURITY | FILE_NOTIFY_CHANGE_LAST_ACCESS) != 0 {
        // Security descriptors and access timestamps are not represented in the guest VFS.
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(INVALID_HANDLE);
    }
    let raw_path = if wide {
        p.read_wstr(path_ptr)?
    } else {
        p.read_cstr(path_ptr)?
    };
    if raw_path.is_empty() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(INVALID_HANDLE);
    }
    let absolute_path = absolute(&raw_path, &p.current_directory);
    if !p.vfs.is_dir(&absolute_path) {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(INVALID_HANDLE);
    }
    if p.vfs.has_streamed_assets_below(&absolute_path) && !p.change_notification_coverage_reported {
        p.stderr.extend_from_slice(
            b"[coverage] kernel32!FindFirstChangeNotificationW on streamed assets observes guest VFS mutations; host-folder changes and mapped-file writeback are not connected.\n",
        );
        p.change_notification_coverage_reported = true;
    }
    p.set_last_error(0);
    Ok(p.new_handle(Handle::ChangeNotification {
        path: p.vfs.normalize_path(&absolute_path),
        subtree,
        filter,
        signaled: false,
        changed_after_signal: false,
    }))
}

fn find_next_change_notification(p: &mut Process) -> R {
    let handle = p.arg(0);
    match p.handles.get_mut(&handle) {
        Some(Handle::ChangeNotification {
            signaled,
            changed_after_signal,
            ..
        }) => {
            *signaled = *changed_after_signal;
            *changed_after_signal = false;
            p.set_last_error(0);
            Ok(1)
        }
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
    }
}

fn find_close_change_notification(p: &mut Process) -> R {
    let handle = p.arg(0);
    if matches!(
        p.handles.get(&handle),
        Some(Handle::ChangeNotification { .. })
    ) {
        p.handles.remove(&handle);
        p.set_last_error(0);
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

fn file_attrs(p: &Process, path: &str) -> u64 {
    let path = absolute(path, &p.current_directory);
    p.vfs
        .file_attributes(&path)
        .or_else(|| {
            matches!(p.vfs.normalize_path(&path).as_str(), "windows" | "temp").then_some(0x10)
        })
        .map_or(u32::MAX as u64, u64::from)
}

fn get_computer_name_a(p: &mut Process) -> R {
    get_computer_name(p, false, "ANYWASM")
}

fn get_computer_name_w(p: &mut Process) -> R {
    get_computer_name(p, true, "ANYWASM")
}

fn get_computer_name(p: &mut Process, wide: bool, name: &str) -> R {
    let (buffer, size_ptr) = (p.arg(0), p.arg(1));
    if size_ptr == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let size = p.rd32(size_ptr)? as usize;
    let required = name.encode_utf16().count() + 1;
    if size < required {
        p.wr32(size_ptr, required as u32)?;
        p.set_last_error(ERROR_BUFFER_OVERFLOW);
        return Ok(0);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if wide {
        p.write_wstr(buffer, name)?;
    } else {
        p.write_cstr(buffer, name)?;
    }
    p.wr32(size_ptr, (required - 1) as u32)?;
    p.set_last_error(0);
    Ok(1)
}

fn get_computer_name_ex_a(p: &mut Process) -> R {
    let (format, buffer, size_ptr) = (p.arg(0) as u32, p.arg(1), p.arg(2));
    if size_ptr == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    // The runtime has one deterministic host identity and no configured DNS domain.
    let name = match format {
        0 | 1 | 4 | 5 => "ANYWASM",
        2 | 6 => "",
        3 | 7 => "ANYWASM",
        _ => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
    };
    let size = p.rd32(size_ptr)? as usize;
    let required = name.len() + 1;
    if size < required {
        p.wr32(size_ptr, required as u32)?;
        p.set_last_error(ERROR_MORE_DATA);
        return Ok(0);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.write_cstr(buffer, name)?;
    p.wr32(size_ptr, (required - 1) as u32)?;
    p.set_last_error(0);
    Ok(1)
}

fn virtual_path_exists(p: &Process, absolute_path: &str) -> bool {
    absolute_path.eq_ignore_ascii_case("C:\\")
        || p.vfs.exists(absolute_path)
        || p.vfs.is_dir(absolute_path)
        || matches!(
            p.vfs.normalize_path(absolute_path).as_str(),
            "windows" | "temp"
        )
}

fn missing_path_error(p: &mut Process, absolute_path: &str) {
    let parent = absolute_path
        .rfind('\\')
        .map(|separator| {
            if separator == 2 && absolute_path.as_bytes().get(1) == Some(&b':') {
                &absolute_path[..=separator]
            } else {
                &absolute_path[..separator]
            }
        })
        .unwrap_or("");
    p.set_last_error(if virtual_path_exists(p, parent) {
        ERROR_FILE_NOT_FOUND
    } else {
        ERROR_PATH_NOT_FOUND
    });
}

fn get_drive_type_a(p: &mut Process) -> R {
    const DRIVE_NO_ROOT_DIR: u64 = 1;
    const DRIVE_FIXED: u64 = 3;
    let input = p.arg(0);
    let explicit_path = input != 0;
    let path = if !explicit_path {
        p.current_directory.clone()
    } else {
        p.read_cstr(input)?
    };
    let has_required_root_separator = !explicit_path || path.ends_with('\\');
    let absolute_path = absolute(&path, &p.current_directory);
    Ok(
        if has_required_root_separator
            && virtual_path_exists(p, &absolute_path)
            && absolute_path
                .get(..2)
                .is_some_and(|drive| drive.eq_ignore_ascii_case("C:"))
        {
            DRIVE_FIXED
        } else {
            DRIVE_NO_ROOT_DIR
        },
    )
}

fn get_logical_drive_strings_a(p: &mut Process) -> R {
    // The virtual machine exposes one fixed volume. The final extra NUL makes this a
    // MULTI_SZ: "C:\\0\0". On success the count excludes that extra NUL.
    const MULTI_SZ: &[u8] = b"C:\\\0\0";
    let (capacity, buffer) = (p.arg(0) as usize, p.arg(1));
    if capacity < MULTI_SZ.len() {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(MULTI_SZ.len() as u64);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.write_bytes(buffer, MULTI_SZ)?;
    p.set_last_error(0);
    Ok((MULTI_SZ.len() - 1) as u64)
}

fn get_volume_information(p: &mut Process, wide: bool) -> R {
    let root_arg = p.arg(0);
    let root = if root_arg == 0 {
        p.current_directory.clone()
    } else if wide {
        p.read_wstr(root_arg)?
    } else {
        p.read_cstr(root_arg)?
    };
    if root.is_empty() {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let root = absolute(&root, &p.current_directory);
    if !root
        .get(..2)
        .is_some_and(|drive| drive.eq_ignore_ascii_case("C:"))
        || !virtual_path_exists(p, &root)
    {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }

    const VOLUME_LABEL: &str = "AnyWASM";
    const FILE_SYSTEM: &str = "ANYWASM";
    const VOLUME_SERIAL: u32 = 0x4157_4153; // "AWAS", stable across runs.
    const MAX_COMPONENT_LENGTH: u32 = 255;
    const FILE_UNICODE_ON_DISK: u32 = 0x4;
    let (volume_buffer, volume_capacity) = (p.arg(1), p.arg(2) as usize);
    let (serial_out, max_component_out, flags_out, fs_buffer, fs_capacity) =
        (p.arg(3), p.arg(4), p.arg(5), p.arg(6), p.arg(7) as usize);
    if (volume_buffer != 0 && volume_capacity < VOLUME_LABEL.len() + 1)
        || (fs_buffer != 0 && fs_capacity < FILE_SYSTEM.len() + 1)
    {
        p.set_last_error(ERROR_MORE_DATA);
        return Ok(0);
    }
    if volume_buffer != 0 {
        if wide {
            p.write_wstr(volume_buffer, VOLUME_LABEL)?;
        } else {
            p.write_cstr(volume_buffer, VOLUME_LABEL)?;
        }
    }
    if fs_buffer != 0 {
        if wide {
            p.write_wstr(fs_buffer, FILE_SYSTEM)?;
        } else {
            p.write_cstr(fs_buffer, FILE_SYSTEM)?;
        }
    }
    if serial_out != 0 {
        p.wr32(serial_out, VOLUME_SERIAL)?;
    }
    if max_component_out != 0 {
        p.wr32(max_component_out, MAX_COMPONENT_LENGTH)?;
    }
    if flags_out != 0 {
        p.wr32(flags_out, FILE_UNICODE_ON_DISK)?;
    }
    p.set_last_error(0);
    Ok(1)
}

fn get_disk_free_space_ex(p: &mut Process) -> R {
    // VFS files are indexed/overlaid without a modeled capacity or user quota, so reporting
    // synthetic byte totals would be indistinguishable from a successful but false query.
    p.set_last_error(ERROR_NOT_SUPPORTED);
    Ok(0)
}

fn get_long_path_name_w(p: &mut Process) -> R {
    let (input, buffer, capacity) = (p.arg(0), p.arg(1), p.arg(2) as usize);
    if input == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let path = p.read_wstr(input)?;
    if path.is_empty() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let absolute_path = absolute(&path, &p.current_directory);
    if !virtual_path_exists(p, &absolute_path) {
        missing_path_error(p, &absolute_path);
        return Ok(0);
    }
    // The VFS does not maintain DOS 8.3 aliases. Existing names are already their long
    // names; absolute() only resolves them against the process working directory.
    let required = absolute_path.encode_utf16().count() + 1;
    if capacity < required {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(required as u64);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.write_wstr(buffer, &absolute_path)?;
    p.set_last_error(0);
    Ok((required - 1) as u64)
}

fn get_file_attributes_ex_w(p: &mut Process) -> R {
    let (path_arg, info_level, output) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if info_level != 0 || path_arg == 0 || output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let path = p.read_wstr(path_arg)?;
    if path.is_empty() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let absolute_path = absolute(&path, &p.current_directory);
    let attributes = file_attrs(p, &absolute_path) as u32;
    if attributes == u32::MAX {
        missing_path_error(p, &absolute_path);
        return Ok(0);
    }
    let size = if attributes & 0x10 != 0 {
        0
    } else {
        p.vfs.file_len(&absolute_path).unwrap_or(0)
    };
    // WIN32_FILE_ATTRIBUTE_DATA has three FILETIME values. VFS entries currently carry no
    // timestamp metadata, represented by zero FILETIMEs; attributes and logical size are real.
    let mut data = [0u8; 36];
    data[0..4].copy_from_slice(&attributes.to_le_bytes());
    data[28..32].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
    data[32..36].copy_from_slice(&(size as u32).to_le_bytes());
    p.write_bytes(output, &data)?;
    p.set_last_error(0);
    Ok(1)
}

fn system_time_to_file_time(p: &mut Process) -> R {
    let (system_time, file_time_out) = (p.arg(0), p.arg(1));
    if system_time == 0 || file_time_out == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let fields = p.read_bytes(system_time, 16)?;
    let mut words = [0u16; 8];
    for (index, pair) in fields.chunks_exact(2).enumerate() {
        words[index] = u16::from_le_bytes([pair[0], pair[1]]);
    }
    let [year, month, _day_of_week, day, hour, minute, second, milliseconds] = words;
    let year = year as u64;
    let month = month as usize;
    if !(1601..=30_827).contains(&year) || !(1..=12).contains(&month) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0
        || day as usize > month_lengths[month - 1]
        || hour >= 24
        || minute >= 60
        || second >= 60
        || milliseconds >= 1000
    {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let days_before_year = |year: u64| {
        let prior = year - 1;
        365 * prior + prior / 4 - prior / 100 + prior / 400
    };
    let mut days = days_before_year(year) - days_before_year(1601);
    days += month_lengths[..month - 1]
        .iter()
        .map(|&length| length as u64)
        .sum::<u64>();
    days += day as u64 - 1;
    let seconds = (((days * 24 + hour as u64) * 60 + minute as u64) * 60) + second as u64;
    let ticks = seconds * 10_000_000 + milliseconds as u64 * 10_000;
    p.wr64(file_time_out, ticks)?;
    p.set_last_error(0);
    Ok(1)
}

fn set_file_attrs(p: &mut Process, path: &str, mut attributes: u32) -> R {
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const SETTABLE_ATTRIBUTES: u32 = 0x3127; // READONLY | HIDDEN | SYSTEM | ARCHIVE | TEMPORARY | OFFLINE | NOT_CONTENT_INDEXED

    if path.is_empty() {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let absolute_path = absolute(path, &p.current_directory);
    let key = p.vfs.normalize_path(&absolute_path);
    let directory = p.vfs.is_dir(&absolute_path) || matches!(key.as_str(), "windows" | "temp");
    if !directory && !p.vfs.exists(&absolute_path) {
        p.set_last_error(ERROR_FILE_NOT_FOUND);
        return Ok(0);
    }
    if attributes == 0 || attributes & !(SETTABLE_ATTRIBUTES | FILE_ATTRIBUTE_NORMAL) != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if attributes & SETTABLE_ATTRIBUTES != 0 {
        attributes &= !FILE_ATTRIBUTE_NORMAL;
    }
    if directory {
        attributes |= FILE_ATTRIBUTE_DIRECTORY;
    }
    let changed = p.vfs.file_attributes(&absolute_path) != Some(attributes);
    p.vfs.set_file_attributes(&absolute_path, attributes);
    if changed {
        p.notify_vfs_change(&absolute_path, FILE_NOTIFY_CHANGE_ATTRIBUTES);
    }
    Ok(1)
}

fn remove_directory_w(p: &mut Process) -> R {
    const ERROR_DIR_NOT_EMPTY: u32 = 145;
    const ERROR_DIRECTORY: u32 = 267;
    let path_ptr = p.arg(0);
    if path_ptr == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let raw_path = p.read_wstr(path_ptr)?;
    let absolute_path = absolute(&raw_path, &p.current_directory);
    let path = p.vfs.normalize_path(&absolute_path);
    let current_directory = p.vfs.normalize_path(&p.current_directory);
    if path.eq_ignore_ascii_case(&current_directory) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    match p.vfs.remove_dir(&absolute_path) {
        Ok(()) => Ok(1),
        Err(RemoveDirError::NotEmpty) => {
            p.set_last_error(ERROR_DIR_NOT_EMPTY);
            Ok(0)
        }
        Err(RemoveDirError::AccessDenied) => {
            p.set_last_error(ERROR_ACCESS_DENIED);
            Ok(0)
        }
        Err(RemoveDirError::NotFound) => {
            p.set_last_error(if p.vfs.file_len(&absolute_path).is_some() {
                ERROR_DIRECTORY
            } else {
                ERROR_PATH_NOT_FOUND
            });
            Ok(0)
        }
    }
}

fn delete_file(p: &mut Process, path: &str) -> R {
    let absolute_path = absolute(path, &p.current_directory);
    if p.vfs.is_readonly(&absolute_path) || p.vfs.is_dir(&absolute_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    if p.vfs.remove(&absolute_path) {
        p.notify_vfs_change(&absolute_path, FILE_NOTIFY_CHANGE_FILE_NAME);
        Ok(1)
    } else {
        p.set_last_error(ERROR_FILE_NOT_FOUND);
        Ok(0)
    }
}

fn file_metadata(p: &Process, path: &str) -> Option<FileMetadata> {
    let status = p.vfs.file_status(path)?;
    Some(FileMetadata {
        accessed: status.accessed,
        modified: status.modified,
        created: status.created,
        mode: status.mode,
        link_count: status.link_count,
    })
}

fn cancel_io(p: &mut Process) -> R {
    match p.handles.get(&p.arg(0)) {
        Some(Handle::File { .. }) => {
            // File operations in this runtime complete synchronously, so a valid file handle
            // has no outstanding operation for CancelIo to cancel.
            p.set_last_error(ERROR_NOT_FOUND);
        }
        _ => p.set_last_error(ERROR_INVALID_HANDLE),
    }
    Ok(0)
}

fn format_message_a(p: &mut Process) -> R {
    const ALLOCATE_BUFFER: u32 = 0x0000_0100;
    const IGNORE_INSERTS: u32 = 0x0000_0200;
    const FROM_STRING: u32 = 0x0000_0400;
    const FROM_MODULE: u32 = 0x0000_0800;
    const FROM_SYSTEM: u32 = 0x0000_1000;
    const ARGUMENT_ARRAY: u32 = 0x0000_2000;
    let (flags, source, message_id, language, output, capacity) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2) as u32,
        p.arg(3) as u32,
        p.arg(4),
        p.arg(5) as u32 as usize,
    );
    let supported_flags =
        ALLOCATE_BUFFER | IGNORE_INSERTS | FROM_STRING | FROM_MODULE | FROM_SYSTEM | ARGUMENT_ARRAY;
    if flags & !supported_flags != 0 || flags & 0xff != 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    let from_string = flags & FROM_STRING != 0;
    let from_system = flags & FROM_SYSTEM != 0;
    if from_string as u8 + from_system as u8 + (flags & FROM_MODULE != 0) as u8 != 1 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    let text = if from_string {
        if source == 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
        let mut bytes = Vec::new();
        for offset in 0..=64 * 1024u64 {
            let byte = p.rd8(source + offset)?;
            if byte == 0 {
                break;
            }
            if offset == 64 * 1024 {
                p.set_last_error(ERROR_INVALID_PARAMETER);
                return Ok(0);
            }
            bytes.push(byte);
        }
        String::from_utf16_lossy(&cp_decode(1252, &bytes))
    } else if flags & FROM_MODULE != 0 {
        p.set_last_error(ERROR_MR_MID_NOT_FOUND);
        return Ok(0);
    } else {
        if language != 0 && language != 0x0409 {
            p.set_last_error(ERROR_RESOURCE_LANG_NOT_FOUND);
            return Ok(0);
        }
        let Some(message) = system_message(message_id) else {
            p.set_last_error(ERROR_MR_MID_NOT_FOUND);
            return Ok(0);
        };
        message.to_owned()
    };
    let formatted = match format_message_escapes(&text, flags & IGNORE_INSERTS != 0) {
        Ok(value) => value,
        Err(stop) => return Err(stop),
    };
    let mut bytes = cp_encode(1252, &formatted.encode_utf16().collect::<Vec<_>>());
    if bytes.is_empty() {
        p.set_last_error(ERROR_MR_MID_NOT_FOUND);
        return Ok(0);
    }
    if bytes.len() >= 64 * 1024 {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    let allocate = flags & ALLOCATE_BUFFER != 0;
    let output_buffer = if allocate {
        if output == 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
        let allocation_size = bytes.len().max(capacity).saturating_add(1) as u64;
        let buffer = alloc_block(p, allocation_size, false);
        if buffer == 0 {
            p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return Ok(0);
        }
        p.wrptr(output, buffer)?;
        buffer
    } else {
        if output == 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
        if capacity < bytes.len() + 1 {
            p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
            return Ok(0);
        }
        output
    };
    let result_length = bytes.len();
    bytes.push(0);
    p.write_bytes(output_buffer, &bytes)?;
    Ok(result_length as u64)
}

fn system_message(code: u32) -> Option<&'static str> {
    Some(match code {
        0 => "The operation completed successfully.\r\n",
        1 => "Incorrect function.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough memory resources are available to process this command.\r\n",
        17 => "The system cannot move the file to a different disk drive.\r\n",
        50 => "The request is not supported.\r\n",
        87 => "The parameter is incorrect.\r\n",
        111 => "The file name is too long.\r\n",
        122 => "The data area passed to a system call is too small.\r\n",
        123 => "The filename, directory name, or volume label syntax is incorrect.\r\n",
        183 => "Cannot create a file when that file already exists.\r\n",
        234 => "More data is available.\r\n",
        _ => return None,
    })
}

fn format_message_escapes(input: &str, ignore_inserts: bool) -> Result<String, Stop> {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '%' || index + 1 == chars.len() {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        match chars[index + 1] {
            '%' => output.push('%'),
            'n' => output.push_str("\r\n"),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            'b' => output.push(' '),
            '0' => break,
            c if c.is_ascii_digit() => {
                if !ignore_inserts {
                    return Err(Stop::Unimplemented {
                        dll: "kernel32".into(),
                        name: "FormatMessageA insertion arguments".into(),
                    });
                }
                output.push('%');
                output.push(c);
            }
            c => {
                output.push('%');
                output.push(c);
            }
        }
        index += 2;
    }
    Ok(output)
}

fn copy_file(p: &mut Process, source: &str, destination: &str, fail_if_exists: bool) -> R {
    let source_path = absolute(source, &p.current_directory);
    let destination_path = absolute(destination, &p.current_directory);
    if source.is_empty() || destination.is_empty() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if source_path.eq_ignore_ascii_case(&destination_path) {
        p.set_last_error(ERROR_FILE_EXISTS);
        return Ok(0);
    }
    if p.vfs.is_dir(&source_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let Some(bytes) = p.vfs.get(&source_path).cloned() else {
        p.set_last_error(if p.vfs.exists(&source_path) {
            ERROR_NOT_SUPPORTED
        } else {
            ERROR_FILE_NOT_FOUND
        });
        return Ok(0);
    };
    let destination_exists = p.vfs.exists(&destination_path);
    if destination_exists && fail_if_exists {
        p.set_last_error(ERROR_FILE_EXISTS);
        return Ok(0);
    }
    if p.vfs.is_dir(&destination_path) || p.vfs.is_readonly(&destination_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let parent = destination_path
        .rsplit_once('\\')
        .map_or("", |(parent, _)| parent);
    if !p.vfs.is_dir(parent) {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let attributes = p.vfs.file_attributes(&source_path).unwrap_or(0x80);
    let metadata = file_metadata(p, &source_path).unwrap_or_default();
    p.vfs.insert(&destination_path, bytes);
    p.vfs.set_file_attributes(&destination_path, attributes);
    p.vfs.set_file_metadata(&destination_path, metadata);
    p.notify_vfs_change(
        &destination_path,
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
    );
    Ok(1)
}

fn move_file(p: &mut Process, source: &str, destination: &str, flags: u32) -> R {
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_COPY_ALLOWED: u32 = 0x2;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    if flags & !(MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH) != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let source_path = absolute(source, &p.current_directory);
    let destination_path = absolute(destination, &p.current_directory);
    if source.is_empty() || destination.is_empty() {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if source_path.eq_ignore_ascii_case(&destination_path) {
        return Ok(1);
    }
    if p.vfs.is_dir(&source_path) || p.vfs.is_readonly(&source_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let Some(bytes) = p.vfs.get(&source_path).cloned() else {
        p.set_last_error(if p.vfs.exists(&source_path) {
            ERROR_NOT_SUPPORTED
        } else {
            ERROR_FILE_NOT_FOUND
        });
        return Ok(0);
    };
    let source_drive = source_path
        .get(..2)
        .filter(|drive| drive.as_bytes().get(1) == Some(&b':'));
    let destination_drive = destination_path
        .get(..2)
        .filter(|drive| drive.as_bytes().get(1) == Some(&b':'));
    if source_drive
        .zip(destination_drive)
        .is_some_and(|(a, b)| !a.eq_ignore_ascii_case(b))
        && flags & MOVEFILE_COPY_ALLOWED == 0
    {
        p.set_last_error(ERROR_NOT_SAME_DEVICE);
        return Ok(0);
    }
    let destination_exists = p.vfs.exists(&destination_path);
    if destination_exists && flags & MOVEFILE_REPLACE_EXISTING == 0 {
        p.set_last_error(ERROR_ALREADY_EXISTS);
        return Ok(0);
    }
    if p.vfs.is_dir(&destination_path) || p.vfs.is_readonly(&destination_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let parent = destination_path
        .rsplit_once('\\')
        .map_or("", |(parent, _)| parent);
    if !p.vfs.is_dir(parent) {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let attributes = p.vfs.file_attributes(&source_path).unwrap_or(0x80);
    let metadata = file_metadata(p, &source_path).unwrap_or_default();
    if destination_exists && !p.vfs.remove(&destination_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    if !p.vfs.remove(&source_path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    p.vfs.insert(&destination_path, bytes);
    p.vfs.set_file_attributes(&destination_path, attributes);
    p.vfs.set_file_metadata(&destination_path, metadata);
    p.notify_vfs_change(&source_path, FILE_NOTIFY_CHANGE_FILE_NAME);
    p.notify_vfs_change(
        &destination_path,
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
    );
    Ok(1)
}

fn set_file_time(p: &mut Process) -> R {
    let handle = p.arg(0);
    let (path, writable) = match p.handles.get(&handle) {
        Some(Handle::File { path, write, .. }) => (path.clone(), *write),
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(0);
        }
    };
    if !writable {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let Some(mut metadata) = file_metadata(p, &path) else {
        p.set_last_error(ERROR_FILE_NOT_FOUND);
        return Ok(0);
    };
    let (creation, access, write) = (p.arg(1), p.arg(2), p.arg(3));
    if creation != 0 {
        let value = p.rd64(creation)?;
        if value != 0 && value != u64::MAX {
            metadata.created = Some(value as i64);
        }
    }
    if access != 0 {
        let value = p.rd64(access)?;
        if value != 0 && value != u64::MAX {
            metadata.accessed = Some(value as i64);
        }
    }
    if write != 0 {
        let value = p.rd64(write)?;
        if value != 0 && value != u64::MAX {
            metadata.modified = Some(value as i64);
        }
    }
    p.vfs.set_file_metadata(&path, metadata);
    p.notify_vfs_change(&path, FILE_NOTIFY_CHANGE_LAST_WRITE);
    Ok(1)
}

fn profile_int(p: &mut Process, section: &str, key: &str, default: u32, path: &str) -> R {
    let path = absolute(path, &p.current_directory);
    let Some(len) = p.vfs.file_len(&path) else {
        return Ok(default as u64);
    };
    let len = usize::try_from(len).map_err(|_| Stop::Crash("profile file is too large".into()))?;
    let bytes = p
        .vfs
        .read_range(&path, 0, len)
        .map_err(|error| Stop::Crash(format!("profile file read failed: {error:?}")))?;
    let mut in_section = false;
    for line in String::from_utf8_lossy(&bytes).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line[1..line.len() - 1].trim().eq_ignore_ascii_case(section);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(key) {
            continue;
        }
        let value = value.trim();
        let parsed = if let Some(hex) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            i64::from_str_radix(hex, 16).ok()
        } else {
            value.parse::<i64>().ok()
        };
        return Ok(parsed.map(|n| n as u32).unwrap_or(default) as u64);
    }
    Ok(default as u64)
}

fn get_private_profile_int_a(p: &mut Process) -> R {
    let (section, key, default, file) = (
        p.read_cstr(p.arg(0))?,
        p.read_cstr(p.arg(1))?,
        p.arg(2) as u32,
        p.read_cstr(p.arg(3))?,
    );
    profile_int(p, &section, &key, default, &file)
}

fn get_private_profile_int_w(p: &mut Process) -> R {
    let (section, key, default, file) = (
        p.read_wstr(p.arg(0))?,
        p.read_wstr(p.arg(1))?,
        p.arg(2) as u32,
        p.read_wstr(p.arg(3))?,
    );
    profile_int(p, &section, &key, default, &file)
}

fn create_directory(p: &mut Process, path: &str) -> R {
    if path.is_empty() {
        p.set_last_error(ERROR_PATH_NOT_FOUND);
        return Ok(0);
    }
    let absolute_path = absolute(path, &p.current_directory);
    match p.vfs.create_dir(&absolute_path) {
        Ok(()) => {
            p.notify_vfs_change(
                &absolute_path,
                FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_CREATION,
            );
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
    let trace = p.handles.get(&h).and_then(|handle| match handle {
        Handle::File { path, pos, .. }
            if p.trace_gta_eventvol && path.ends_with("models\\particle.txd") =>
        {
            Some((path.clone(), *pos))
        }
        _ => None,
    });
    let new = seek(p, h, off, method)?;
    if let Some((path, before)) = trace {
        let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
        p.stderr.extend_from_slice(
            format!(
                "[gta-file-seek] path={path:?} before={before:#x} offset={off:#x} method={method} result={new:?} caller={caller:#x}\n"
            )
            .as_bytes(),
        );
    }
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
    let (h, off, out, method) = if p.cpu.mode == CpuMode::X86_32 {
        let low = p.arg(1) as u32 as u64;
        let high = p.arg(2) as u32 as i32 as i64;
        (p.arg(0), (high << 32) | low as i64, p.arg(3), p.arg(4))
    } else {
        (p.arg(0), p.arg(1) as i64, p.arg(2), p.arg(3))
    };
    if method > 2 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let trace = p.handles.get(&h).and_then(|handle| match handle {
        Handle::File { path, pos, .. }
            if p.trace_gta_eventvol && path.ends_with("models\\particle.txd") =>
        {
            Some((path.clone(), *pos))
        }
        _ => None,
    });
    let cache_trace = if p.trace_imports {
        p.handles.get(&h).and_then(|handle| match handle {
            Handle::File { path, pos, .. } if path.ends_with("caspartcache.package") => {
                Some((path.clone(), *pos))
            }
            _ => None,
        })
    } else {
        None
    };
    let result = seek(p, h, off, method)?;
    if let Some((path, before)) = trace {
        let caller = p.mem.read_u32(p.cpu.gpr[RSP]).unwrap_or(0);
        p.stderr.extend_from_slice(
            format!(
                "[gta-file-seek] api=SetFilePointerEx path={path:?} before={before:#x} offset={off:#x} method={method} result={result:?} caller={caller:#x}\n"
            )
            .as_bytes(),
        );
    }
    if let Some((path, before)) = cache_trace {
        p.stderr.extend_from_slice(
            format!(
                "[cache-file-seek] path={path:?} before={before:#x} offset={off:#x} method={method} result={result:?}\n"
            )
            .as_bytes(),
        );
    }
    match result {
        Some(n) => {
            if out != 0 {
                p.wr64(out, n)?;
            }
            Ok(1)
        }
        None => Ok(0),
    }
}
fn set_end_of_file(p: &mut Process) -> R {
    let handle = p.arg(0);
    let (path, position, writable) = match p.handles.get(&handle) {
        Some(Handle::File {
            path, pos, write, ..
        }) => (path.clone(), *pos, *write),
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            return Ok(0);
        }
    };
    if !writable {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    if p.vfs.is_readonly(&path) {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let length = position;
    if p.trace_imports && path.ends_with("caspartcache.package") {
        p.stderr.extend_from_slice(
            format!("[cache-file-eof] path={path:?} length={length:#x}\n").as_bytes(),
        );
    }
    let previous_len = p.vfs.file_len(&path).unwrap_or(0) as usize;
    let Some(file) = p.vfs.get_mut(&path) else {
        p.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    };
    file.resize(length, 0);
    if previous_len != length {
        p.notify_vfs_change(
            &path,
            FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
        );
    }
    p.set_last_error(0);
    Ok(1)
}

fn flush_file_buffers(p: &mut Process) -> R {
    let handle = p.arg(0);
    match p.handles.get(&handle) {
        Some(Handle::File {
            path, write: true, ..
        }) if !p.vfs.is_readonly(path) => {
            // VFS writes are committed immediately to the in-memory overlay.
            p.set_last_error(0);
            Ok(1)
        }
        Some(Handle::File { .. }) => {
            p.set_last_error(ERROR_ACCESS_DENIED);
            Ok(0)
        }
        _ => {
            p.set_last_error(ERROR_INVALID_HANDLE);
            Ok(0)
        }
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
    command_line_a(p)
}
pub(crate) fn command_line_a(p: &mut Process) -> R {
    if p.extra.cmdline_a == 0 {
        let s = p.cmdline.clone();
        let a = alloc_block(p, s.len() as u64 + 1, true);
        p.write_cstr(a, &s)?;
        p.extra.cmdline_a = a;
    }
    Ok(p.extra.cmdline_a)
}
fn get_command_line_w(p: &mut Process) -> R {
    command_line_w(p)
}
pub(crate) fn command_line_w(p: &mut Process) -> R {
    if p.extra.cmdline_w == 0 {
        let s = p.cmdline.clone();
        let units = s.encode_utf16().count() as u64;
        let a = alloc_block(p, (units + 1) * 2, true);
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
            let length = if wide {
                v.encode_utf16().count()
            } else {
                v.len()
            };
            if length + 1 <= n {
                if wide {
                    p.write_wstr(buf, &v)?;
                } else {
                    p.write_cstr(buf, &v)?;
                }
                p.set_last_error(0);
                Ok(length as u64)
            } else {
                p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
                Ok(length as u64 + 1)
            }
        }
        None => {
            p.set_last_error(203);
            Ok(0)
        }
    }
}

fn set_environment_variable(p: &mut Process, wide: bool) -> R {
    let (name_pointer, value_pointer) = (p.arg(0), p.arg(1));
    if name_pointer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let name = if wide {
        p.read_wstr(name_pointer)?
    } else {
        p.read_cstr(name_pointer)?
    };
    if name.is_empty() || name.contains('=') {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if value_pointer == 0 {
        p.env.retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
    } else {
        let value = if wide {
            p.read_wstr(value_pointer)?
        } else {
            p.read_cstr(value_pointer)?
        };
        if let Some((_, old_value)) = p
            .env
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            *old_value = value;
        } else {
            p.env.push((name, value));
        }
    }
    crt::refresh_environment(p)?;
    p.set_last_error(0);
    Ok(1)
}

fn free_environment_strings(p: &mut Process, wide: bool) -> R {
    let block = p.arg(0);
    let Some(size) = p.heap.size_of(block) else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    let bytes = p.read_bytes(block, size as usize)?;
    let valid = if wide {
        if bytes.len() % 2 != 0 {
            false
        } else {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            validate_environment_multisz(&units)
        }
    } else {
        validate_environment_multisz(&bytes.iter().map(|&byte| byte as u16).collect::<Vec<_>>())
    };
    if !valid || !free_block(p, block) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    p.set_last_error(0);
    Ok(1)
}

fn validate_environment_multisz(units: &[u16]) -> bool {
    let Some(end) = units.windows(2).position(|pair| pair == [0, 0]) else {
        return false;
    };
    if units[end + 2..].iter().any(|&unit| unit != 0) {
        return false;
    }
    let data = &units[..end];
    if data.is_empty() {
        return true;
    }
    data.split(|&unit| unit == 0)
        .all(|entry| !entry.is_empty() && entry.contains(&(b'=' as u16)))
}

fn expand_environment_strings_a(p: &mut Process) -> R {
    let (source, destination, capacity) = (p.arg(0), p.arg(1), p.arg(2) as usize);
    if source == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let input = p.read_cstr(source)?;
    let chars: Vec<char> = input.chars().collect();
    let mut expanded = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < chars.len() {
        if chars[cursor] != '%' {
            expanded.push(chars[cursor]);
            cursor += 1;
            continue;
        }
        let Some(relative_end) = chars[cursor + 1..].iter().position(|&ch| ch == '%') else {
            expanded.extend(chars[cursor..].iter().copied());
            break;
        };
        let end = cursor + 1 + relative_end;
        let name: String = chars[cursor + 1..end].iter().collect();
        if name.is_empty() {
            expanded.push('%');
        } else if let Some((_, value)) = p
            .env
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            expanded.push_str(value);
        } else {
            expanded.extend(chars[cursor..=end].iter().copied());
        }
        cursor = end + 1;
    }
    let required = expanded.len().saturating_add(1);
    if capacity == 0 || capacity < required {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(required as u64);
    }
    if destination == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.write_cstr(destination, &expanded)?;
    p.set_last_error(0);
    Ok(required as u64)
}

// modules ----------------------------------------------------------------------------------

fn free_library(p: &mut Process) -> R {
    if p.module_by_base(p.arg(0)).is_some() {
        // Loaded modules stay resident because import thunks and dependency bookkeeping retain
        // their addresses for the process lifetime. FreeLibrary is therefore a reference-count
        // no-op after validating the module handle.
        p.set_last_error(0);
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

fn disable_thread_library_calls(p: &mut Process) -> R {
    if p.module_by_base(p.arg(0)).is_some() {
        // The scheduler does not deliver DLL_THREAD_ATTACH/DETACH notifications.
        p.set_last_error(0);
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

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
    if p.trace_imports || p.trace_modules_only {
        p.stderr
            .extend_from_slice(format!("[module] LoadLibrary {n}\n").as_bytes());
    }
    if let Some(i) = p.module_by_name(&n) {
        return Ok(p.modules[i].base);
    }
    if anywasm_pe::canonical_dll(crate::process::base_name(&n)) == "dinput8" {
        let i = p.register_hle_module(&n, "dinput8");
        return Ok(p.modules[i].base);
    }
    let relative = absolute(&n, &p.current_directory);
    let file = if p.vfs.exists(&relative) {
        Some(p.vfs.normalize_path(&relative))
    } else if p.vfs.exists(&n) {
        Some(norm(&n))
    } else {
        p.vfs.find_by_name(crate::process::base_name(&n)).cloned()
    };
    if let Some(f) = file {
        let data = p.vfs.get(&f).unwrap().clone();
        return match p.load_image(&data, &f, true) {
            Ok(i) => {
                // Initialize the new dependency graph in Windows load order. Existing modules
                // are already marked initialized and are skipped by dll_init_order.
                for module in p.dll_init_order(i) {
                    p.run_dll_process_attach(module)?;
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
    if p.trace_imports || p.trace_modules_only {
        p.stderr
            .extend_from_slice(format!("[module] GetProcAddress {}!{name}\n", m.name).as_bytes());
    }
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

fn system_time_fields(p: &Process, address: u64) -> Result<Option<[u16; 8]>, Stop> {
    let fields = if address == 0 {
        [2026, 1, 4, 1, 0, 0, 0, 0]
    } else {
        let bytes = p.read_bytes(address, 16)?;
        let mut words = [0u16; 8];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            words[index] = u16::from_le_bytes([pair[0], pair[1]]);
        }
        words
    };
    let [year, month, day_of_week, day, hour, minute, second, milliseconds] = fields;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1601..=30_827).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > month_days[month as usize - 1]
        || day_of_week > 6
        || hour > 23
        || minute > 59
        || second > 59
        || milliseconds > 999
    {
        return Ok(None);
    }
    Ok(Some(fields))
}

fn format_date_pattern(pattern: &str, fields: [u16; 8]) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const WEEKDAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let mut output = String::new();
    let mut chars = pattern.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            quoted = !quoted;
            continue;
        }
        if quoted || !matches!(ch, 'd' | 'M' | 'y') {
            output.push(ch);
            continue;
        }
        let mut count = 1;
        while chars.peek() == Some(&ch) {
            chars.next();
            count += 1;
        }
        match ch {
            'd' => match count {
                1 => output.push_str(&fields[3].to_string()),
                2 => output.push_str(&format!("{:02}", fields[3])),
                3 => output.push_str(&WEEKDAYS[fields[2] as usize][..3]),
                _ => output.push_str(WEEKDAYS[fields[2] as usize]),
            },
            'M' => match count {
                1 => output.push_str(&fields[1].to_string()),
                2 => output.push_str(&format!("{:02}", fields[1])),
                3 => output.push_str(&MONTHS[fields[1] as usize - 1][..3]),
                _ => output.push_str(MONTHS[fields[1] as usize - 1]),
            },
            'y' if count <= 2 => output.push_str(&format!("{:02}", fields[0] % 100)),
            'y' => output.push_str(&format!("{:04}", fields[0])),
            _ => unreachable!(),
        }
    }
    output
}

fn format_time_pattern(pattern: &str, fields: [u16; 8]) -> String {
    let mut output = String::new();
    let mut chars = pattern.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            quoted = !quoted;
            continue;
        }
        if quoted || !matches!(ch, 'h' | 'H' | 'm' | 's' | 't') {
            output.push(ch);
            continue;
        }
        let mut count = 1;
        while chars.peek() == Some(&ch) {
            chars.next();
            count += 1;
        }
        match ch {
            'h' => {
                let hour = match fields[4] % 12 {
                    0 => 12,
                    value => value,
                };
                output.push_str(&if count == 1 {
                    hour.to_string()
                } else {
                    format!("{hour:02}")
                });
            }
            'H' => output.push_str(&if count == 1 {
                fields[4].to_string()
            } else {
                format!("{:02}", fields[4])
            }),
            'm' => output.push_str(&if count == 1 {
                fields[5].to_string()
            } else {
                format!("{:02}", fields[5])
            }),
            's' => output.push_str(&if count == 1 {
                fields[6].to_string()
            } else {
                format!("{:02}", fields[6])
            }),
            't' if count == 1 => output.push(if fields[4] < 12 { 'A' } else { 'P' }),
            't' => output.push_str(if fields[4] < 12 { "AM" } else { "PM" }),
            _ => unreachable!(),
        }
    }
    output
}

fn write_format_a(p: &mut Process, text: &str, output: u64, capacity: i32) -> R {
    if capacity < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut bytes = cp_encode(1252, &text.encode_utf16().collect::<Vec<_>>());
    bytes.push(0);
    if capacity == 0 {
        return Ok(bytes.len() as u64);
    }
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if (capacity as usize) < bytes.len() {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    p.write_bytes(output, &bytes)?;
    p.set_last_error(0);
    Ok(bytes.len() as u64)
}

/// Fail at the unsupported service boundary instead of returning an invented Win32 result.
/// These entries make the missing subsystem explicit to the guest and analyzer alike.
fn unsupported_kernel32(api: &'static str, reason: &'static str) -> R {
    Err(Stop::Unimplemented {
        dll: "kernel32".into(),
        name: format!("{api}: {reason}"),
    })
}

const GEOCLASS_NATION: u32 = 16;
const GEOCLASS_REGION: u32 = 14;
const GEOID_US: u32 = 244;

fn get_user_geo_id(p: &mut Process) -> R {
    match p.arg(0) as u32 {
        // This runtime's virtual locale profile is English (United States).
        GEOCLASS_NATION => Ok(GEOID_US as u64),
        // Region IDs are host/user-configured and are not part of the virtual locale profile.
        GEOCLASS_REGION => unsupported_kernel32(
            "GetUserGeoID",
            "the user-region GEOID is not configured in the virtual locale profile",
        ),
        // GEOID_NOT_AVAILABLE is the documented result for unsupported classes.
        _ => Ok(u32::MAX as u64),
    }
}

fn get_geo_info_a(p: &mut Process) -> R {
    let (location, geo_type, output, capacity, langid) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as i32,
        p.arg(4) as u16,
    );
    if capacity < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if location == 0 || location == u32::MAX || location >= 0x8000_0000 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if location != GEOID_US {
        return unsupported_kernel32(
            "GetGeoInfoA",
            "geo data is only populated for the runtime's United States locale profile",
        );
    }

    // Values and character counts match the Windows GEO_* table for GEOID 244. The fixed profile
    // uses LANGID 0x0409; zero asks for the same default language.
    let value = match geo_type {
        1 | 18 => "244",                 // GEO_NATION / GEO_ID
        2 => "39.450",                   // GEO_LATITUDE
        3 => "-98.908",                  // GEO_LONGITUDE
        4 => "US",                       // GEO_ISO2
        5 => "USA",                      // GEO_ISO3
        6 => "en-us",                    // GEO_RFC1766
        7 => "00000409",                 // GEO_LCID
        8 => "United States",            // GEO_FRIENDLYNAME
        9 => "United States of America", // GEO_OFFICIALNAME
        12 => "840",                     // GEO_ISO_UN_NUMBER
        14 => "1",                       // GEO_DIALINGCODE
        15 => "USD",                     // GEO_CURRENCYCODE
        16 => "$",                       // GEO_CURRENCYSYMBOL
        17 => "US",                      // GEO_NAME
        10 | 11 | 13 => {
            return unsupported_kernel32(
                "GetGeoInfoA",
                "timezone, official-language, and parent-region datasets are not modeled",
            );
        }
        _ => {
            p.set_last_error(ERROR_INVALID_FLAGS);
            return Ok(0);
        }
    };
    if langid != 0 && langid != 0x0409 {
        return unsupported_kernel32(
            "GetGeoInfoA",
            "localized geo names are only available for the virtual English (United States) locale",
        );
    }

    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    let required = bytes.len();
    // Like Windows, a null/zero buffer is a size query and returns the required ANSI character
    // count including the terminator.
    if capacity == 0 {
        return Ok(required as u64);
    }
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if (capacity as usize) < required {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    p.write_bytes(output, &bytes)?;
    Ok(required as u64)
}

fn get_date_format_a(p: &mut Process) -> R {
    const DATE_SHORTDATE: u32 = 1;
    const DATE_LONGDATE: u32 = 2;
    const DATE_USE_ALT_CALENDAR: u32 = 4;
    const DATE_YEARMONTH: u32 = 8;
    const DATE_LTRREADING: u32 = 0x10;
    const DATE_RTLREADING: u32 = 0x20;
    let (locale, flags, system_time, format, output, capacity) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3),
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    if !format_locale_is_en_us(p, locale, "GetDateFormatA")? {
        return Ok(0);
    }
    let date_style =
        flags & (DATE_SHORTDATE | DATE_LONGDATE | DATE_USE_ALT_CALENDAR | DATE_YEARMONTH);
    if flags & !(0x3f) != 0
        || (date_style.count_ones() > 1)
        || flags & DATE_LTRREADING != 0 && flags & DATE_RTLREADING != 0
    {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    if flags & DATE_USE_ALT_CALENDAR != 0 {
        p.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    let Some(fields) = system_time_fields(p, system_time)? else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let pattern = if format != 0 {
        p.read_cstr(format)?
    } else {
        match date_style {
            DATE_LONGDATE => "dddd, MMMM d, yyyy".into(),
            DATE_YEARMONTH => "MMMM yyyy".into(),
            _ => "M/d/yyyy".into(),
        }
    };
    let text = format_date_pattern(&pattern, fields);
    write_format_a(p, &text, output, capacity)
}

fn get_time_format_a(p: &mut Process) -> R {
    const TIME_NOMINUTESORSECONDS: u32 = 1;
    const TIME_NOSECONDS: u32 = 2;
    const TIME_NOTIMEMARKER: u32 = 4;
    const TIME_FORCE24HOURFORMAT: u32 = 8;
    let (locale, flags, system_time, format, output, capacity) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3),
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    if !format_locale_is_en_us(p, locale, "GetTimeFormatA")? {
        return Ok(0);
    }
    if flags & !0x0f != 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    let Some(fields) = system_time_fields(p, system_time)? else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let pattern = if format != 0 {
        p.read_cstr(format)?
    } else if flags & TIME_FORCE24HOURFORMAT != 0 {
        "HH:mm:ss".into()
    } else if flags & TIME_NOMINUTESORSECONDS != 0 {
        if flags & TIME_NOTIMEMARKER != 0 {
            "h".into()
        } else {
            "h tt".into()
        }
    } else if flags & TIME_NOSECONDS != 0 {
        if flags & TIME_NOTIMEMARKER != 0 {
            "h:mm".into()
        } else {
            "h:mm tt".into()
        }
    } else if flags & TIME_NOTIMEMARKER != 0 {
        "h:mm:ss".into()
    } else {
        "h:mm:ss tt".into()
    };
    let text = format_time_pattern(&pattern, fields);
    write_format_a(p, &text, output, capacity)
}

fn format_locale_is_en_us(p: &mut Process, locale: u32, api: &str) -> Result<bool, Stop> {
    if matches!(locale, 0x0409 | 0x0400 | 0x0800 | 0x007f) {
        return Ok(true);
    }
    if locale & 0xfff0_0000 != 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(false);
    }
    Err(Stop::Crash(format!(
        "untranslated: kernel32!{api} has no locale profile for LCID {locale:#010x}"
    )))
}

fn is_valid_locale(p: &mut Process) -> R {
    const LCID_INSTALLED: u32 = 1;
    const LCID_SUPPORTED: u32 = 2;
    let (locale, flags) = (p.arg(0) as u32, p.arg(1) as u32);
    if flags & !(LCID_INSTALLED | LCID_SUPPORTED) != 0 || flags == 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    p.set_last_error(0);
    Ok((locale == 0x0409) as u64)
}

fn enum_system_locales_a(p: &mut Process) -> R {
    const LCID_INSTALLED: u32 = 1;
    const LCID_SUPPORTED: u32 = 2;
    let (callback, flags) = (p.arg(0), p.arg(1) as u32);
    if callback == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if flags & !(LCID_INSTALLED | LCID_SUPPORTED) != 0 || flags == 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    let locale_string = alloc_block(p, 9, true);
    if locale_string == 0 {
        p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    p.write_bytes(locale_string, b"00000409\0")?;
    let callback_result = p.call_guest(callback, &[locale_string]);
    free_block(p, locale_string);
    match callback_result {
        Ok(0) => Ok(0),
        Ok(_) => Ok(1),
        Err(stop) => Err(stop),
    }
}

fn get_disk_free_space(p: &mut Process) -> R {
    // The virtual VFS does not expose total or free volume capacity, so all four output values
    // remain untouched and the query fails rather than reporting fabricated cluster counts.
    p.set_last_error(ERROR_NOT_SUPPORTED);
    Ok(0)
}

fn set_std_handle(p: &mut Process) -> R {
    let index = match p.arg(0) as u32 {
        0xffff_fff6 => 0,
        0xffff_fff5 => 1,
        0xffff_fff4 => 2,
        _ => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
    };
    p.std_handles[index] = p.arg(1);
    p.set_last_error(0);
    Ok(1)
}

fn set_console_ctrl_handler(p: &mut Process) -> R {
    let (handler, add) = (p.arg(0), p.arg(1) != 0);
    if handler == 0 {
        p.ignore_ctrl_c = add;
        p.set_last_error(0);
        return Ok(1);
    }
    if add {
        p.console_ctrl_handlers.push(handler);
        p.set_last_error(0);
        return Ok(1);
    }
    let Some(index) = p
        .console_ctrl_handlers
        .iter()
        .rposition(|registered| *registered == handler)
    else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    p.console_ctrl_handlers.remove(index);
    p.set_last_error(0);
    Ok(1)
}

fn fatal_app_exit_a(p: &mut Process) -> R {
    let action = p.arg(0) as u32;
    let message = p.read_cstr(p.arg(1))?;
    if action != 0 {
        return Err(Stop::Crash(format!(
            "untranslated: kernel32!FatalAppExitA action={action:#x}"
        )));
    }
    Err(Stop::Unimplemented {
        dll: "kernel32".into(),
        name: format!(
            "FatalAppExitA requires a modal system message box before termination; message={message:?}"
        ),
    })
}

fn get_system_time_adjustment(p: &mut Process) -> R {
    let (adjustment, increment, disabled) = (p.arg(0), p.arg(1), p.arg(2));
    if adjustment == 0 || increment == 0 || disabled == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    // The virtual clock advances without periodic Windows clock correction.
    p.wr32(adjustment, 10_000)?;
    p.wr32(increment, 10_000)?;
    p.wr32(disabled, 1)?;
    p.set_last_error(0);
    Ok(1)
}

// strings ----------------------------------------------------------------------------------

fn is_supported_code_page(code_page: u32) -> bool {
    matches!(code_page, 437 | 1252 | 65001)
}

fn conversion_code_page(code_page: u32) -> u32 {
    if code_page == 0 {
        VIRTUAL_ANSI_CODE_PAGE // CP_ACP resolves to GetACP(), but is not itself a registered page ID.
    } else {
        code_page
    }
}

fn is_valid_code_page(p: &mut Process) -> R {
    Ok(is_supported_code_page(p.arg(0) as u32) as u64)
}

fn get_cp_info(p: &mut Process) -> R {
    let (code_page, output) = (p.arg(0) as u32, p.arg(1));
    if !is_supported_code_page(code_page) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut info = [0u8; 20];
    let max_char_size = if code_page == 65001 { 4u32 } else { 1 };
    info[..4].copy_from_slice(&max_char_size.to_le_bytes());
    info[4] = b'?';
    p.write_bytes(output, &info)?;
    p.set_last_error(0);
    Ok(1)
}

fn get_default_locale_name(p: &mut Process) -> R {
    const LOCALE_NAME: &str = "en-US";
    let (buffer, capacity) = (p.arg(0), p.arg(1) as usize);
    let required = LOCALE_NAME.encode_utf16().count() + 1;
    if capacity < required {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    if buffer == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    p.write_wstr(buffer, LOCALE_NAME)?;
    p.set_last_error(0);
    Ok(required as u64)
}

fn get_locale_info(p: &mut Process, wide: bool) -> R {
    const LOCALE_IDEFAULTANSICODEPAGE: u32 = 0x0000_1004;
    const LOCALE_USER_DEFAULT: u32 = 0x0000_0400;
    const LOCALE_SYSTEM_DEFAULT: u32 = 0x0000_0800;
    const LOCALE_INVARIANT: u32 = 0x0000_007f;
    const LOCALE_NOUSEROVERRIDE: u32 = 0x8000_0000;
    const LOCALE_USE_CP_ACP: u32 = 0x4000_0000;
    const LOCALE_RETURN_NUMBER: u32 = 0x2000_0000;
    const ALLOWED_FLAGS: u32 = LOCALE_NOUSEROVERRIDE | LOCALE_USE_CP_ACP | LOCALE_RETURN_NUMBER;

    let (locale, raw_type, output, raw_capacity) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as u32 as i32,
    );
    if raw_capacity < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if raw_type & 0xf000_0000 & !ALLOWED_FLAGS != 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }

    let lctype = raw_type & !ALLOWED_FLAGS;
    if lctype == 0 {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    if lctype != LOCALE_IDEFAULTANSICODEPAGE {
        return Err(Stop::Crash(format!(
            "untranslated: kernel32!GetLocaleInfo{} LCType {lctype:#010x} for LCID {locale:#010x}",
            if wide { "W" } else { "A" }
        )));
    }

    // The emulated Windows profile reports en-US for both default locale APIs.
    // The invariant and zh-SG entries are separately observed Windows NLS values.
    let code_page = match locale {
        0x0000_0409 | LOCALE_USER_DEFAULT | LOCALE_SYSTEM_DEFAULT | LOCALE_INVARIANT => 1252,
        0x0000_1004 => 936, // zh-SG; this is an LCID here, distinct from the LCTYPE above.
        _ if locale & 0xfff0_0000 != 0 => {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
        _ => {
            return Err(Stop::Crash(format!(
            "untranslated: kernel32!GetLocaleInfo{} has no locale profile for LCID {locale:#010x}",
            if wide { "W" } else { "A" }
        )))
        }
    };

    let return_number = raw_type & LOCALE_RETURN_NUMBER != 0;
    let required = if return_number {
        if wide {
            2
        } else {
            4
        }
    } else {
        code_page.to_string().len() + 1
    };
    if raw_capacity == 0 {
        return Ok(required as u64);
    }
    if output == 0 || (raw_capacity as usize) < required {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }

    if return_number {
        p.wr32(output, code_page)?;
    } else if wide {
        p.write_wstr(output, &code_page.to_string())?;
    } else {
        let units = code_page.to_string().encode_utf16().collect::<Vec<_>>();
        let mut bytes = cp_encode(1252, &units);
        bytes.push(0);
        p.write_bytes(output, &bytes)?;
    }
    Ok(required as u64)
}

fn is_dbcs_lead_byte_ex(p: &mut Process) -> R {
    let code_page = p.arg(0) as u32;
    if !is_supported_code_page(code_page) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    // The runtime exposes only single-byte ACP/OEM pages and UTF-8.
    Ok(0)
}

fn is_dbcs_lead_byte(_p: &mut Process) -> R {
    // The current ANSI code page is fixed to Windows-1252, which has no DBCS lead bytes.
    Ok(0)
}

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
        437 => bytes.iter().map(|&b| cp437(b)).collect(),
        _ => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp437(byte: u8) -> u16 {
    const HIGH: [u16; 128] = [
        0x00c7, 0x00fc, 0x00e9, 0x00e2, 0x00e4, 0x00e0, 0x00e5, 0x00e7, 0x00ea, 0x00eb, 0x00e8,
        0x00ef, 0x00ee, 0x00ec, 0x00c4, 0x00c5, 0x00c9, 0x00e6, 0x00c6, 0x00f4, 0x00f6, 0x00f2,
        0x00fb, 0x00f9, 0x00ff, 0x00d6, 0x00dc, 0x00a2, 0x00a3, 0x00a5, 0x20a7, 0x0192, 0x00e1,
        0x00ed, 0x00f3, 0x00fa, 0x00f1, 0x00d1, 0x00aa, 0x00ba, 0x00bf, 0x2310, 0x00ac, 0x00bd,
        0x00bc, 0x00a1, 0x00ab, 0x00bb, 0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x2561, 0x2562,
        0x2556, 0x2555, 0x2563, 0x2551, 0x2557, 0x255d, 0x255c, 0x255b, 0x2510, 0x2514, 0x2534,
        0x252c, 0x251c, 0x2500, 0x253c, 0x255e, 0x255f, 0x255a, 0x2554, 0x2569, 0x2566, 0x2560,
        0x2550, 0x256c, 0x2567, 0x2568, 0x2564, 0x2565, 0x2559, 0x2558, 0x2552, 0x2553, 0x256b,
        0x256a, 0x2518, 0x250c, 0x2588, 0x2584, 0x258c, 0x2590, 0x2580, 0x03b1, 0x00df, 0x0393,
        0x03c0, 0x03a3, 0x03c3, 0x00b5, 0x03c4, 0x03a6, 0x0398, 0x03a9, 0x03b4, 0x221e, 0x03c6,
        0x03b5, 0x2229, 0x2261, 0x00b1, 0x2265, 0x2264, 0x2320, 0x2321, 0x00f7, 0x2248, 0x00b0,
        0x2219, 0x00b7, 0x221a, 0x207f, 0x00b2, 0x25a0, 0x00a0,
    ];
    if byte < 0x80 {
        byte as u16
    } else {
        HIGH[(byte - 0x80) as usize]
    }
}

pub(crate) fn cp_encode(code_page: u32, units: &[u16]) -> Vec<u8> {
    match code_page {
        65001 => String::from_utf16_lossy(units).into_bytes(),
        437 => units
            .iter()
            .map(|&unit| {
                if unit < 0x80 {
                    unit as u8
                } else {
                    (0x80u8..=0xff)
                        .find(|&byte| cp437(byte) == unit)
                        .unwrap_or(b'?')
                }
            })
            .collect(),
        _ => units
            .iter()
            .map(|&unit| {
                if unit < 0x100 && !(0x80..0xa0).contains(&unit) {
                    unit as u8
                } else {
                    (0x80u8..0xa0)
                        .find(|&byte| cp1252(byte) == unit)
                        .unwrap_or(b'?')
                }
            })
            .collect(),
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
    let (requested_cp, _fl, src, n, dst, dn) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    let cp = conversion_code_page(requested_cp);
    if n == 0 || n < -1 || dn < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if !is_supported_code_page(cp) {
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
    let (requested_cp, _fl, src, n, dst, dn) = (
        p.arg(0) as u32,
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    let cp = conversion_code_page(requested_cp);
    if n == 0 || n < -1 || dn < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if !is_supported_code_page(cp) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut w = read_wide_n(p, src, n as i64)?;
    if n < 0 {
        w.push(0);
    }
    let out = cp_encode(cp, &w);
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
    let (info_type, source, count, output) =
        (p.arg(0) as u32, p.arg(1), p.arg(2) as u32 as i32, p.arg(3));
    if !validate_string_type_request(p, "GetStringTypeW", info_type, source, count, output)? {
        return Ok(0);
    }
    let units = read_wide_string_type_source(p, source, count)?;
    write_ctype1(p, output, &units)
}

fn get_string_type_ex_w(p: &mut Process) -> R {
    // GetStringTypeExW adds a locale argument, used only for the corresponding ANSI conversion.
    let (info_type, source, count, output) =
        (p.arg(1) as u32, p.arg(2), p.arg(3) as u32 as i32, p.arg(4));
    if !validate_string_type_request(p, "GetStringTypeExW", info_type, source, count, output)? {
        return Ok(0);
    }
    let units = read_wide_string_type_source(p, source, count)?;
    write_ctype1(p, output, &units)
}

fn get_string_type_a(p: &mut Process) -> R {
    const LOCALE_USER_DEFAULT: u32 = 0x0400;
    const LOCALE_SYSTEM_DEFAULT: u32 = 0x0800;
    const LOCALE_INVARIANT: u32 = 0x007f;

    let (locale, info_type, source, count, output) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
    );
    if !validate_string_type_request(p, "GetStringTypeA", info_type, source, count, output)? {
        return Ok(0);
    }
    if !matches!(
        locale,
        0x0409 | LOCALE_USER_DEFAULT | LOCALE_SYSTEM_DEFAULT | LOCALE_INVARIANT
    ) {
        if locale & 0xfff0_0000 != 0 {
            p.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(0);
        }
        return Err(Stop::Crash(format!(
            "untranslated: kernel32!GetStringTypeA has no ANSI code-page profile for LCID {locale:#010x}"
        )));
    }
    if source == 0 || output == 0 || source == output || count == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let bytes = if count < 0 {
        let mut bytes = p
            .mem
            .read_cstr(source, 1 << 20)
            .map_err(|error| Stop::Crash(format!("read fault {source:#x}: {error:?}")))?;
        bytes.push(0);
        bytes
    } else {
        p.read_bytes(source, count as usize)?
    };
    let units = cp_decode(1252, &bytes);
    write_ctype1(p, output, &units)
}

fn validate_string_type_request(
    p: &mut Process,
    api: &str,
    info_type: u32,
    source: u64,
    count: i32,
    output: u64,
) -> Result<bool, Stop> {
    if info_type != 1 {
        if matches!(info_type, 2 | 3) {
            return Err(Stop::Crash(format!(
                "untranslated: kernel32!{api} CT_CTYPE{info_type}"
            )));
        }
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(false);
    }
    if source == 0 || output == 0 || source == output || count == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(false);
    }
    Ok(true)
}

fn read_wide_string_type_source(p: &Process, source: u64, count: i32) -> Result<Vec<u16>, Stop> {
    if source == 0 || count == 0 {
        return Err(Stop::Crash(
            "GetStringType received a null source or zero character count".into(),
        ));
    }
    if count < 0 {
        let mut units = Vec::new();
        for index in 0..(1 << 20) {
            let address = source + index * 2;
            let unit = p.rd16(address)?;
            units.push(unit);
            if unit == 0 {
                return Ok(units);
            }
        }
        return Err(Stop::Crash(format!(
            "GetStringTypeW source at {source:#x} is not null-terminated within 1 Mi UTF-16 units"
        )));
    }
    let byte_count = (count as usize)
        .checked_mul(2)
        .ok_or_else(|| Stop::Crash("GetStringTypeW source length overflow".into()))?;
    let bytes = p.read_bytes(source, byte_count)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

fn write_ctype1(p: &mut Process, output: u64, units: &[u16]) -> R {
    for (index, &unit) in units.iter().enumerate() {
        let byte_index = unit as usize * 2;
        let flags =
            u16::from_le_bytes([WINDOWS_CTYPE1[byte_index], WINDOWS_CTYPE1[byte_index + 1]]);
        p.wr16(output + index as u64 * 2, flags)?;
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

fn lc_map_string_a(p: &mut Process) -> R {
    const LCMAP_LOWERCASE: u32 = 0x0000_0100;
    const LCMAP_UPPERCASE: u32 = 0x0000_0200;
    let (locale, flags, source, source_len, destination, destination_len) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
        p.arg(5) as u32 as i32,
    );
    if !format_locale_is_en_us(p, locale, "LCMapStringA")? {
        return Ok(0);
    }
    if source_len == 0 || source_len < -1 || destination_len < 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if source == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if flags & !(LCMAP_LOWERCASE | LCMAP_UPPERCASE) != 0
        || flags == (LCMAP_LOWERCASE | LCMAP_UPPERCASE)
    {
        p.set_last_error(ERROR_INVALID_FLAGS);
        return Ok(0);
    }
    let mut bytes = if source_len < 0 {
        let mut source = p
            .mem
            .read_cstr(source, 1 << 20)
            .map_err(|error| Stop::Crash(format!("{error:?}")))?;
        source.push(0);
        source
    } else {
        p.read_bytes(source, source_len as usize)?
    };
    let mut units = cp_decode(1252, &bytes);
    if flags & LCMAP_UPPERCASE != 0 {
        for unit in &mut units {
            *unit = char::from_u32(*unit as u32)
                .and_then(|character| character.to_uppercase().next())
                .map(|character| character as u32 as u16)
                .unwrap_or(*unit);
        }
    } else if flags & LCMAP_LOWERCASE != 0 {
        for unit in &mut units {
            *unit = char::from_u32(*unit as u32)
                .and_then(|character| character.to_lowercase().next())
                .map(|character| character as u32 as u16)
                .unwrap_or(*unit);
        }
    }
    bytes = cp_encode(1252, &units);
    if destination_len == 0 {
        return Ok(bytes.len() as u64);
    }
    if destination == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if (destination_len as usize) < bytes.len() {
        p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    p.write_bytes(destination, &bytes)?;
    p.set_last_error(0);
    Ok(bytes.len() as u64)
}

#[cfg(test)]
mod lc_map_string_tests {
    use super::*;
    use anywasm_cpu::cpu::{CpuMode, R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    fn invoke_x86(p: &mut Process, name: &str, args: &[u32]) -> Result<u64, Stop> {
        const STACK: u64 = 0x80800;
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(name).expect("kernel32 API should resolve")(p)
    }

    #[test]
    fn lstrcat_a_appends_ansi_bytes_and_returns_the_original_destination() {
        const BASE: u64 = 0x80000;
        const DEST: u64 = BASE + 0x100;
        const SOURCE: u64 = BASE + 0x200;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.write_bytes(DEST, b"old\0\0").unwrap();
        p.write_bytes(SOURCE, &[0xe9, b'!', 0]).unwrap();
        p.cpu.gpr[RCX] = DEST;
        p.cpu.gpr[RDX] = SOURCE;

        let lstrcat_a = lookup("lstrcatA").expect("kernel32!lstrcatA should resolve");
        assert_eq!(lstrcat_a(&mut p), Ok(DEST));
        assert_eq!(
            p.read_bytes(DEST, 6).unwrap(),
            [b'o', b'l', b'd', 0xe9, b'!', 0]
        );
    }

    #[test]
    fn initialize_slist_head_clears_the_pe32_or_pe32_plus_header_size() {
        const BASE: u64 = 0x90000;
        const HEADER: u64 = BASE + 0x100;
        const STACK: u64 = BASE + 0x800;
        let initialize = lookup("InitializeSListHead").unwrap();

        for (mode, size) in [(CpuMode::X86_32, 8usize), (CpuMode::X86_64, 16usize)] {
            let mut p = Process::new(crate::vfs::Vfs::new());
            assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
            p.cpu.mode = mode;
            p.cpu.gpr[RSP] = STACK;
            p.write_bytes(HEADER, &[0xa5; 24]).unwrap();
            if mode == CpuMode::X86_32 {
                p.wr32(STACK, 0).unwrap();
                p.wr32(STACK + 4, HEADER as u32).unwrap();
            } else {
                p.cpu.gpr[RCX] = HEADER;
            }

            assert_eq!(initialize(&mut p), Ok(0));
            let bytes = p.read_bytes(HEADER, 24).unwrap();
            assert!(bytes[..size].iter().all(|byte| *byte == 0));
            assert!(bytes[size..].iter().all(|byte| *byte == 0xa5));
        }
    }

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
    fn vectored_exception_handlers_follow_first_last_order_and_remove_by_token() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let add = lookup("AddVectoredExceptionHandler")
            .expect("AddVectoredExceptionHandler should resolve");
        let remove = lookup("RemoveVectoredExceptionHandler")
            .expect("RemoveVectoredExceptionHandler should resolve");

        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 0x1400_01000;
        let last = add(&mut p).unwrap();
        p.cpu.gpr[RCX] = 1;
        p.cpu.gpr[RDX] = 0x1400_02000;
        let first = add(&mut p).unwrap();

        assert_ne!(last, 0);
        assert_ne!(first, 0);
        assert_ne!(last, first);
        assert_eq!(
            p.vectored_exception_handlers,
            vec![(first, 0x1400_02000), (last, 0x1400_01000)]
        );
        p.cpu.gpr[RCX] = last;
        assert_eq!(remove(&mut p), Ok(1));
        assert_eq!(p.vectored_exception_handlers, vec![(first, 0x1400_02000)]);
        p.cpu.gpr[RCX] = last;
        assert_eq!(remove(&mut p), Ok(0));
    }

    #[test]
    fn vectored_continue_handlers_use_a_separate_ordered_registry() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let add_veh = lookup("AddVectoredExceptionHandler").unwrap();
        let add_vch = lookup("AddVectoredContinueHandler").unwrap();
        let remove_vch = lookup("RemoveVectoredContinueHandler").unwrap();

        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 0x1400_01000;
        let veh = add_veh(&mut p).unwrap();
        p.cpu.gpr[RCX] = 0;
        p.cpu.gpr[RDX] = 0x1400_02000;
        let last = add_vch(&mut p).unwrap();
        p.cpu.gpr[RCX] = 1;
        p.cpu.gpr[RDX] = 0x1400_03000;
        let first = add_vch(&mut p).unwrap();

        assert_ne!(veh, last);
        assert_ne!(last, first);
        assert_eq!(p.vectored_exception_handlers, vec![(veh, 0x1400_01000)]);
        assert_eq!(
            p.vectored_continue_handlers,
            vec![(first, 0x1400_03000), (last, 0x1400_02000)]
        );
        p.cpu.gpr[RCX] = last;
        assert_eq!(remove_vch(&mut p), Ok(1));
        assert_eq!(p.vectored_continue_handlers, vec![(first, 0x1400_03000)]);
        p.cpu.gpr[RCX] = veh;
        assert_eq!(remove_vch(&mut p), Ok(0));
    }

    #[test]
    fn set_unhandled_exception_filter_returns_and_replaces_the_previous_filter() {
        let mut p = Process::new(crate::vfs::Vfs::new());
        let set_filter = lookup("SetUnhandledExceptionFilter").unwrap();

        p.cpu.gpr[RCX] = 0x1400_01000;
        assert_eq!(set_filter(&mut p), Ok(0));
        p.cpu.gpr[RCX] = 0x1400_02000;
        assert_eq!(set_filter(&mut p), Ok(0x1400_01000));
        p.cpu.gpr[RCX] = 0;
        assert_eq!(set_filter(&mut p), Ok(0x1400_02000));
    }

    #[test]
    fn startup_info_uses_the_guest_bitness_layout_and_preserves_adjacent_stack_data() {
        const BASE: u64 = 0x40000;
        const BUFFER: u64 = BASE + 0x100;
        const STACK: u64 = BASE + 0x800;
        for (mode, expected_size) in [(CpuMode::X86_32, 68u32), (CpuMode::X86_64, 104u32)] {
            let mut p = Process::new(crate::vfs::Vfs::new());
            assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
            p.cpu.mode = mode;
            p.cpu.gpr[RSP] = STACK;
            p.mem.write_force(BUFFER, &[0xa5; 128]).unwrap();
            if mode == CpuMode::X86_32 {
                p.wr32(STACK, 0).unwrap();
                p.wr32(STACK + 4, BUFFER as u32).unwrap();
            } else {
                p.cpu.gpr[RCX] = BUFFER;
            }

            lookup("GetStartupInfoA").unwrap()(&mut p).unwrap();

            assert_eq!(p.rd32(BUFFER).unwrap(), expected_size);
            assert_eq!(p.mem.read_u8(BUFFER + expected_size as u64).unwrap(), 0xa5);
        }
    }

    #[test]
    fn disk_free_space_queries_fail_without_fabricating_capacity() {
        const BASE: u64 = 0x50000;
        const STACK: u64 = BASE + 0x800;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        let arguments = [0, BASE + 0x100, BASE + 0x104, BASE + 0x108, BASE + 0x10c];
        p.wr32(STACK, 0).unwrap();
        for (index, argument) in arguments.into_iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, argument as u32)
                .unwrap();
        }
        let query = lookup("GetDiskFreeSpaceA").expect("GetDiskFreeSpaceA should resolve");
        assert_eq!(query(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_NOT_SUPPORTED);
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

    #[test]
    fn multi_byte_to_wide_char_resolves_cp_acp_to_the_virtual_ansi_page() {
        const BASE: u64 = 0x80000;
        const SOURCE: u64 = BASE + 0x100;
        const OUTPUT: u64 = BASE + 0x200;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.write_bytes(SOURCE, &[0xe9, 0]).unwrap();

        assert_eq!(invoke_x86(&mut p, "GetACP", &[]), Ok(1252));
        assert_eq!(
            invoke_x86(
                &mut p,
                "MultiByteToWideChar",
                &[0, 0, SOURCE as u32, u32::MAX, 0, 0]
            ),
            Ok(2)
        );
        assert_eq!(
            invoke_x86(
                &mut p,
                "MultiByteToWideChar",
                &[0, 0, SOURCE as u32, u32::MAX, OUTPUT as u32, 2]
            ),
            Ok(2)
        );
        assert_eq!(p.read_bytes(OUTPUT, 4).unwrap(), [0xe9, 0, 0, 0]);
    }

    #[test]
    fn wide_char_to_multi_byte_resolves_cp_acp_to_the_virtual_ansi_page() {
        const BASE: u64 = 0x80000;
        const SOURCE: u64 = BASE + 0x100;
        const OUTPUT: u64 = BASE + 0x200;
        let mut p = Process::new(crate::vfs::Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.write_wstr(SOURCE, "é").unwrap();

        assert_eq!(
            invoke_x86(
                &mut p,
                "WideCharToMultiByte",
                &[0, 0, SOURCE as u32, u32::MAX, 0, 0, 0, 0]
            ),
            Ok(2)
        );
        assert_eq!(
            invoke_x86(
                &mut p,
                "WideCharToMultiByte",
                &[0, 0, SOURCE as u32, u32::MAX, OUTPUT as u32, 2, 0, 0]
            ),
            Ok(2)
        );
        assert_eq!(p.read_bytes(OUTPUT, 2).unwrap(), [0xe9, 0]);
    }
}

#[cfg(test)]
mod priority_class_tests {
    use super::{lookup, ERROR_INVALID_HANDLE, THREAD_PRIORITY_ERROR_RETURN};
    use crate::process::{Process, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    #[test]
    fn get_priority_class_returns_normal_for_the_current_process() {
        const BASE: u64 = 0x80000;
        const STACK: u64 = BASE + 0x800;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        p.wr32(STACK + 4, u32::MAX).unwrap(); // GetCurrentProcess pseudo-handle

        let get_priority_class =
            lookup("GetPriorityClass").expect("kernel32!GetPriorityClass should resolve");
        assert_eq!(get_priority_class(&mut p), Ok(0x20)); // NORMAL_PRIORITY_CLASS

        p.wr32(STACK + 8, 0x100).unwrap(); // REALTIME_PRIORITY_CLASS
        let set_priority_class =
            lookup("SetPriorityClass").expect("kernel32!SetPriorityClass should resolve");
        assert_eq!(set_priority_class(&mut p), Ok(1));
        assert_eq!(get_priority_class(&mut p), Ok(0x100));
    }

    #[test]
    fn thread_priority_queries_and_updates_the_current_thread_state() {
        const BASE: u64 = 0x81000;
        const STACK: u64 = BASE + 0x800;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        p.wr32(STACK + 4, u32::MAX - 1).unwrap(); // GetCurrentThread pseudo-handle

        let get = lookup("GetThreadPriority").expect("GetThreadPriority should be translated");
        let set = lookup("SetThreadPriority").expect("SetThreadPriority should be translated");
        assert_eq!(get(&mut p), Ok(0)); // THREAD_PRIORITY_NORMAL

        p.wr32(STACK + 8, 15).unwrap(); // THREAD_PRIORITY_TIME_CRITICAL
        assert_eq!(set(&mut p), Ok(1));
        assert_eq!(get(&mut p), Ok(15));

        p.wr32(STACK + 8, 3).unwrap(); // Not one of the supported priority levels.
        assert_eq!(set(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 87); // ERROR_INVALID_PARAMETER
    }

    #[test]
    fn realtime_thread_priority_extensions_and_invalid_handles_follow_win32_errors() {
        const BASE: u64 = 0x82000;
        const STACK: u64 = BASE + 0x800;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        p.wr32(STACK + 4, u32::MAX - 1).unwrap();

        let set = lookup("SetThreadPriority").expect("SetThreadPriority should be translated");
        let get = lookup("GetThreadPriority").expect("GetThreadPriority should be translated");
        p.wr32(STACK + 8, 3).unwrap();
        assert_eq!(set(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 87); // Invalid outside realtime class.

        p.wr32(STACK + 4, u32::MAX).unwrap();
        p.wr32(STACK + 8, 0x100).unwrap();
        let set_class = lookup("SetPriorityClass").unwrap();
        assert_eq!(set_class(&mut p), Ok(1));
        p.wr32(STACK + 4, u32::MAX - 1).unwrap();
        p.wr32(STACK + 8, 3).unwrap();
        assert_eq!(set(&mut p), Ok(1));
        assert_eq!(get(&mut p), Ok(3));

        p.wr32(STACK + 4, 0xdead_beef).unwrap();
        assert_eq!(get(&mut p), Ok(THREAD_PRIORITY_ERROR_RETURN as u64));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
        p.wr32(STACK + 8, 0).unwrap();
        assert_eq!(set(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
    }
}

#[cfg(test)]
mod sleep_ex_tests {
    use super::lookup;
    use crate::process::{Process, ThreadState, ThreadWait};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    #[test]
    fn alertable_sleep_ex_without_queued_apcs_blocks_until_its_deadline() {
        const BASE: u64 = 0x90000;
        const STACK: u64 = BASE + 0x800;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        p.wr32(STACK + 4, 50).unwrap();
        p.wr32(STACK + 8, 1).unwrap();
        let deadline = p.now_ticks() + 5_000_000;

        let sleep_ex = lookup("SleepEx").expect("kernel32!SleepEx should resolve");
        assert_eq!(sleep_ex(&mut p), Ok(0));
        assert_eq!(
            p.current_thread_state,
            ThreadState::Blocked(ThreadWait::Sleep { deadline })
        );
    }

    #[test]
    fn nested_alertable_sleep_ex_advances_virtual_time_without_blocking() {
        const BASE: u64 = 0xa0000;
        const STACK: u64 = BASE + 0x800;
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        p.wr32(STACK + 4, 50).unwrap();
        p.wr32(STACK + 8, 1).unwrap();
        p.nested = 1;
        let before = p.now_ticks();

        let sleep_ex = lookup("SleepEx").expect("kernel32!SleepEx should resolve");
        assert_eq!(sleep_ex(&mut p), Ok(0));
        assert_eq!(p.current_thread_state, ThreadState::Running);
        assert_eq!(p.now_ticks(), before + 5_000_000);
    }
}

#[cfg(test)]
mod virtual_query_api_tests {
    use super::lookup;
    use crate::process::{Process, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x10000;
    const STACK: u64 = BASE + 0x800;
    const LAST_ERROR: u64 = TEB_ADDR + 0x68;

    fn process(vfs: Vfs) -> Process {
        let mut p = Process::new(vfs);
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u64]) -> u64 {
        let mut registers = [0; 4];
        for (index, value) in args.iter().take(4).enumerate() {
            registers[index] = *value;
        }
        p.cpu.gpr[RCX] = registers[0];
        p.cpu.gpr[RDX] = registers[1];
        p.cpu.gpr[R8] = registers[2];
        p.cpu.gpr[R9] = registers[3];
        for (index, value) in args.iter().enumerate().skip(4) {
            p.wr64(STACK + 8 + index as u64 * 8, *value).unwrap();
        }
        lookup(name).unwrap_or_else(|| panic!("kernel32!{name} should resolve"))(p).unwrap()
    }

    fn write_system_time(p: &mut Process, address: u64, fields: [u16; 8]) {
        let bytes: Vec<u8> = fields.into_iter().flat_map(u16::to_le_bytes).collect();
        p.write_bytes(address, &bytes).unwrap();
    }

    #[test]
    fn computer_name_apis_report_required_sizes_and_virtual_identity() {
        let mut p = process(Vfs::new());
        p.wr32(BASE + 0x100, 4).unwrap();
        assert_eq!(
            call(&mut p, "GetComputerNameW", &[BASE + 0x200, BASE + 0x100]),
            0
        );
        assert_eq!(p.rd32(BASE + 0x100).unwrap(), 8);
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 111); // ERROR_BUFFER_OVERFLOW

        p.wr32(BASE + 0x100, 8).unwrap();
        assert_eq!(
            call(&mut p, "GetComputerNameW", &[BASE + 0x200, BASE + 0x100]),
            1
        );
        assert_eq!(p.read_wstr(BASE + 0x200).unwrap(), "ANYWASM");
        assert_eq!(p.rd32(BASE + 0x100).unwrap(), 7);

        p.wr32(BASE + 0x100, 3).unwrap();
        assert_eq!(
            call(&mut p, "GetComputerNameA", &[BASE + 0x280, BASE + 0x100]),
            0
        );
        assert_eq!(p.rd32(BASE + 0x100).unwrap(), 8);
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 111); // ERROR_BUFFER_OVERFLOW

        p.wr32(BASE + 0x108, 8).unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetComputerNameExA",
                &[4, BASE + 0x300, BASE + 0x108], // ComputerNamePhysicalNetBIOS
            ),
            1
        );
        assert_eq!(p.read_cstr(BASE + 0x300).unwrap(), "ANYWASM");
        assert_eq!(p.rd32(BASE + 0x108).unwrap(), 7);

        p.wr32(BASE + 0x108, 2).unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetComputerNameExA",
                &[1, BASE + 0x300, BASE + 0x108],
            ),
            0
        );
        assert_eq!(p.rd32(BASE + 0x108).unwrap(), 8);
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 234); // ERROR_MORE_DATA
    }

    #[test]
    fn drives_and_volume_information_describe_the_single_virtual_c_volume() {
        let mut vfs = Vfs::new();
        vfs.insert("C:\\Games\\app.exe", vec![1]);
        let mut p = process(vfs);
        assert_eq!(call(&mut p, "GetLogicalDrives", &[]), 4);

        p.write_cstr(BASE + 0x100, "C:\\Games\\").unwrap();
        assert_eq!(call(&mut p, "GetDriveTypeA", &[BASE + 0x100]), 3); // DRIVE_FIXED
        p.write_cstr(BASE + 0x100, "D:\\").unwrap();
        assert_eq!(call(&mut p, "GetDriveTypeA", &[BASE + 0x100]), 1); // DRIVE_NO_ROOT_DIR

        assert_eq!(
            call(&mut p, "GetLogicalDriveStringsA", &[5, BASE + 0x200]),
            4
        );
        assert_eq!(p.read_bytes(BASE + 0x200, 5).unwrap(), b"C:\\\0\0");

        p.write_cstr(BASE + 0x100, "C:\\Games").unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetVolumeInformationA",
                &[
                    BASE + 0x100,
                    BASE + 0x300,
                    16,
                    BASE + 0x340,
                    BASE + 0x344,
                    BASE + 0x348,
                    BASE + 0x380,
                    16,
                ],
            ),
            1
        );
        assert_eq!(p.read_cstr(BASE + 0x300).unwrap(), "AnyWASM");
        assert_eq!(p.rd32(BASE + 0x340).unwrap(), 0x4157_4153);
        assert_eq!(p.rd32(BASE + 0x344).unwrap(), 255);
        assert_eq!(p.rd32(BASE + 0x348).unwrap(), 4); // FILE_UNICODE_ON_DISK
        assert_eq!(p.read_cstr(BASE + 0x380).unwrap(), "ANYWASM");

        p.write_wstr(BASE + 0x400, "C:\\Games").unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetVolumeInformationW",
                &[
                    BASE + 0x400,
                    BASE + 0x500,
                    8,
                    0,
                    0,
                    BASE + 0x540,
                    BASE + 0x580,
                    8,
                ],
            ),
            1
        );
        assert_eq!(p.read_wstr(BASE + 0x500).unwrap(), "AnyWASM");
        assert_eq!(p.read_wstr(BASE + 0x580).unwrap(), "ANYWASM");

        p.wr64(BASE + 0x600, 0xfeed_face).unwrap();
        assert_eq!(
            call(&mut p, "GetLogicalDriveStringsA", &[4, BASE + 0x600]),
            5
        );
        assert_eq!(p.rd64(BASE + 0x600).unwrap(), 0xfeed_face);
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 122); // ERROR_INSUFFICIENT_BUFFER
    }

    #[test]
    fn metadata_path_and_global_size_queries_use_virtual_files_and_heap_blocks() {
        let mut vfs = Vfs::new();
        vfs.insert("C:\\Game\\Config.ini", b"config".to_vec());
        let mut p = process(vfs);

        p.write_wstr(BASE + 0x100, "C:\\Game\\Config.ini").unwrap();
        p.wr16(BASE + 0x300, 0xaaaa).unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetLongPathNameW",
                &[BASE + 0x100, BASE + 0x300, 18],
            ),
            19
        );
        assert_eq!(p.rd16(BASE + 0x300).unwrap(), 0xaaaa);
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 122); // ERROR_INSUFFICIENT_BUFFER
        assert_eq!(
            call(
                &mut p,
                "GetFileAttributesExW",
                &[BASE + 0x100, 0, BASE + 0x200]
            ),
            1
        );
        assert_eq!(p.rd32(BASE + 0x200).unwrap(), 0x80);
        assert_eq!(p.rd32(BASE + 0x200 + 28).unwrap(), 0);
        assert_eq!(p.rd32(BASE + 0x200 + 32).unwrap(), 6);
        assert_eq!(p.rd64(BASE + 0x200 + 4).unwrap(), 0); // VFS stores no timestamps.

        assert_eq!(
            call(
                &mut p,
                "GetLongPathNameW",
                &[BASE + 0x100, BASE + 0x300, 64],
            ),
            18
        );
        assert_eq!(p.read_wstr(BASE + 0x300).unwrap(), "C:\\Game\\Config.ini");

        p.write_wstr(BASE + 0x100, "C:\\Game\\missing.ini").unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetFileAttributesExW",
                &[BASE + 0x100, 0, BASE + 0x200]
            ),
            0
        );
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 2); // ERROR_FILE_NOT_FOUND
        p.write_wstr(BASE + 0x100, "C:\\Missing\\missing.ini")
            .unwrap();
        assert_eq!(
            call(
                &mut p,
                "GetFileAttributesExW",
                &[BASE + 0x100, 0, BASE + 0x200]
            ),
            0
        );
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 3); // ERROR_PATH_NOT_FOUND

        let allocation = call(&mut p, "GlobalAlloc", &[0, 13]);
        assert_ne!(allocation, 0);
        assert_eq!(call(&mut p, "GlobalSize", &[allocation]), 16);
        assert_eq!(call(&mut p, "GlobalSize", &[0xdead_beef]), 0);
        assert_eq!(call(&mut p, "GlobalFree", &[allocation]), 0);
        assert_eq!(call(&mut p, "GlobalSize", &[allocation]), 0);
    }

    #[test]
    fn system_time_conversion_validates_calendar_fields_and_uses_filetime_epoch() {
        let mut p = process(Vfs::new());
        write_system_time(
            &mut p,
            BASE + 0x100,
            [1970, 1, 4, 1, 0, 0, 0, 0], // Day of week is ignored.
        );
        assert_eq!(
            call(
                &mut p,
                "SystemTimeToFileTime",
                &[BASE + 0x100, BASE + 0x200]
            ),
            1
        );
        assert_eq!(p.rd64(BASE + 0x200).unwrap(), 116_444_736_000_000_000);

        write_system_time(&mut p, BASE + 0x100, [2023, 2, 3, 29, 0, 0, 0, 0]);
        assert_eq!(
            call(
                &mut p,
                "SystemTimeToFileTime",
                &[BASE + 0x100, BASE + 0x200]
            ),
            0
        );
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 87); // ERROR_INVALID_PARAMETER
    }

    #[test]
    fn unsupported_disk_capacity_query_fails_explicitly_without_fake_outputs() {
        let mut p = process(Vfs::new());
        p.wr64(BASE + 0x100, 0xaaaa).unwrap();
        assert_eq!(
            call(&mut p, "GetDiskFreeSpaceExA", &[0, BASE + 0x100, 0, 0]),
            0
        );
        assert_eq!(p.rd32(LAST_ERROR).unwrap(), 50); // ERROR_NOT_SUPPORTED
        assert_eq!(p.rd64(BASE + 0x100).unwrap(), 0xaaaa);
        assert_eq!(
            call(&mut p, "GetDiskFreeSpaceExW", &[0, 0, BASE + 0x100, 0]),
            0
        );
        assert_eq!(p.rd64(BASE + 0x100).unwrap(), 0xaaaa);
    }
}

#[cfg(test)]
mod kernel32_stub_audit_tests {
    use super::{
        lookup, ERROR_ACCESS_DENIED, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER,
        ERROR_LOCK_VIOLATION, ERROR_NOT_LOCKED,
    };
    use crate::process::{Handle, Process, Stop, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW, PROT_RWX};

    const BASE: u64 = 0x20000;
    const STACK: u64 = BASE + 0x800;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    #[test]
    fn heap_create_allocates_zeroed_memory_from_its_private_heap_and_destroy_releases_it() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;

        let heap = call_x86(&mut p, "HeapCreate", &[0, 0x1000, 0]) as u32;
        assert_ne!(heap, 0);
        assert_ne!(heap as u64, p.heap.process_heap);

        let block = call_x86(&mut p, "HeapAlloc", &[heap, 0x8, 0x20]) as u32;
        assert_ne!(block, 0);
        assert_eq!(p.rd32(block as u64).unwrap(), 0);
        assert_eq!(call_x86(&mut p, "HeapSize", &[heap, 0, block]), 0x20);
        assert_eq!(call_x86(&mut p, "HeapFree", &[heap, 0, block]), 1);
        assert_eq!(call_x86(&mut p, "HeapDestroy", &[heap]), 1);
        assert!(p.mem.read_u32(block as u64).is_err());

        assert_eq!(call_x86(&mut p, "HeapAlloc", &[heap, 0, 0x20]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
    }

    #[test]
    fn private_heaps_reject_foreign_blocks_and_respect_fixed_capacity() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;

        let first = call_x86(&mut p, "HeapCreate", &[0, 0x1000, 0x1000]) as u32;
        let second = call_x86(&mut p, "HeapCreate", &[0, 0x1000, 0]) as u32;
        assert_ne!(first, 0);
        assert_ne!(second, 0);
        assert_ne!(first, second);

        let first_block = call_x86(&mut p, "HeapAlloc", &[first, 0, 0x100]) as u32;
        assert_ne!(first_block, 0);
        assert_eq!(call_x86(&mut p, "HeapFree", &[second, 0, first_block]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
        assert_eq!(
            call_x86(&mut p, "HeapSize", &[second, 0, first_block]),
            u32::MAX as u64
        );

        assert_eq!(call_x86(&mut p, "HeapAlloc", &[first, 0, 0x2000]), 0);
        assert_eq!(
            p.rd32(TEB_ADDR + 0x34).unwrap(),
            super::ERROR_NOT_ENOUGH_MEMORY
        );
        let process_heap = p.heap.process_heap as u32;
        assert_eq!(call_x86(&mut p, "HeapDestroy", &[process_heap]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
    }

    #[test]
    fn standard_handles_error_mode_and_legacy_handle_count_keep_process_state() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert_eq!(call_x86(&mut p, "GetStdHandle", &[0xffff_fff5]), 0x14);
        assert_eq!(call_x86(&mut p, "SetStdHandle", &[0xffff_fff5, 0x400]), 1);
        assert_eq!(call_x86(&mut p, "GetStdHandle", &[0xffff_fff5]), 0x400);
        assert_eq!(call_x86(&mut p, "SetStdHandle", &[0xffff_fff3, 0x500]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);

        assert_eq!(call_x86(&mut p, "SetErrorMode", &[0x8001]), 0);
        assert_eq!(call_x86(&mut p, "SetErrorMode", &[0x8000]), 0x8001);
        assert_eq!(call_x86(&mut p, "SetHandleCount", &[64]), 64);
    }

    #[test]
    fn console_control_handler_registration_and_ignore_flag_are_stateful() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        assert_eq!(call_x86(&mut p, "SetConsoleCtrlHandler", &[0, 1]), 1);
        assert!(p.ignore_ctrl_c);
        assert_eq!(call_x86(&mut p, "SetConsoleCtrlHandler", &[0, 0]), 1);
        assert!(!p.ignore_ctrl_c);

        assert_eq!(call_x86(&mut p, "SetConsoleCtrlHandler", &[0x1234, 1]), 1);
        assert_eq!(p.console_ctrl_handlers, vec![0x1234]);
        assert_eq!(call_x86(&mut p, "SetConsoleCtrlHandler", &[0x1234, 0]), 1);
        assert!(p.console_ctrl_handlers.is_empty());
        assert_eq!(call_x86(&mut p, "SetConsoleCtrlHandler", &[0x1234, 0]), 0);
    }

    #[test]
    fn ansi_locale_mapping_and_date_time_formatting_follow_buffer_contracts() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let source = (BASE + 0x100) as u32;
        let output = source + 0x100;
        p.write_bytes(source as u64, b"gTa").unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "LCMapStringA",
                &[0x0409, 0x0200, source, 3, output, 8]
            ),
            3
        );
        assert_eq!(p.read_bytes(output as u64, 3).unwrap(), b"GTA");

        assert_eq!(
            call_x86(&mut p, "GetDateFormatA", &[0x0409, 0, 0, 0, 0, 0]),
            9
        );
        assert_eq!(
            call_x86(&mut p, "GetDateFormatA", &[0x0409, 0, 0, 0, output, 9]),
            9
        );
        assert_eq!(p.read_cstr(output as u64).unwrap(), "1/1/2026");

        assert_eq!(
            call_x86(&mut p, "GetTimeFormatA", &[0x0409, 0, 0, 0, 0, 0]),
            12
        );
        assert_eq!(
            call_x86(&mut p, "GetTimeFormatA", &[0x0409, 0, 0, 0, output, 12]),
            12
        );
        assert_eq!(p.read_cstr(output as u64).unwrap(), "12:00:00 AM");
        assert_eq!(call_x86(&mut p, "IsValidLocale", &[0x0409, 1]), 1);
        assert_eq!(call_x86(&mut p, "IsValidLocale", &[0x0407, 1]), 0);
    }

    #[test]
    fn find_resource_w_uses_utf16_names_and_integer_resources() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        add_resource_module(&mut p, true);
        assert_eq!(
            call_x86(&mut p, "FindResourceW", &[0x0040_0000, 101, 14]),
            0x0040_1110
        );
        let wide_group = (BASE + 0x180) as u32;
        p.write_wstr(wide_group as u64, "Group").unwrap();
        assert_eq!(
            call_x86(&mut p, "FindResourceW", &[0x0040_0000, wide_group, 14]),
            0x0040_1100
        );
    }

    #[test]
    fn set_file_pointer_ex_decodes_the_x86_large_integer_and_fifth_move_method() {
        let mut p = process();
        p.vfs.insert("cache.pkg", b"abcdef".to_vec());
        p.handles.insert(
            0x400,
            Handle::File {
                path: "cache.pkg".into(),
                pos: 0,
                write: true,
                overlapped: false,
            },
        );
        let output = (BASE + 0x100) as u32;

        assert_eq!(
            call_x86(&mut p, "SetFilePointerEx", &[0x400, 2, 0, output, 0]),
            1
        );
        assert_eq!(
            p.handles.get(&0x400).and_then(|handle| match handle {
                Handle::File { pos, .. } => Some(*pos),
                _ => None,
            }),
            Some(2)
        );
        assert_eq!(p.rd64(output as u64).unwrap(), 2);

        assert_eq!(
            call_x86(&mut p, "SetFilePointerEx", &[0x400, 1, 0, output, 1]),
            1
        );
        assert_eq!(p.rd64(output as u64).unwrap(), 3);

        assert_eq!(
            call_x86(
                &mut p,
                "SetFilePointerEx",
                &[0x400, u32::MAX, u32::MAX, output, 2],
            ),
            1
        );
        assert_eq!(p.rd64(output as u64).unwrap(), 5);

        p.wr64(output as u64, 0xaabb_ccdd_1122_3344).unwrap();
        assert_eq!(
            call_x86(&mut p, "SetFilePointerEx", &[0x400, 0, 0, output, 3]),
            0
        );
        assert_eq!(p.rd64(output as u64).unwrap(), 0xaabb_ccdd_1122_3344);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
        assert!(matches!(
            p.handles.get(&0x400),
            Some(Handle::File { pos: 5, .. })
        ));
    }

    #[test]
    fn copy_and_move_file_operate_on_local_vfs_files_with_win32_collision_rules() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.current_directory = r"C:\Game".into();
        p.vfs
            .insert(r"C:\Game\source.bin", b"file payload".to_vec());
        let source_a = (BASE + 0x100) as u32;
        let copy_a = (BASE + 0x140) as u32;
        p.write_bytes(source_a as u64, b"source.bin\0").unwrap();
        p.write_bytes(copy_a as u64, b"copy.bin\0").unwrap();

        assert_eq!(call_x86(&mut p, "CopyFileA", &[source_a, copy_a, 1]), 1);
        assert_eq!(p.vfs.get(r"C:\Game\source.bin").unwrap(), b"file payload");
        assert_eq!(p.vfs.get(r"C:\Game\copy.bin").unwrap(), b"file payload");

        let source_w = (BASE + 0x180) as u32;
        let copy_w = (BASE + 0x1c0) as u32;
        p.write_wstr(source_w as u64, "source.bin").unwrap();
        p.write_wstr(copy_w as u64, "copyw.bin").unwrap();
        assert_eq!(call_x86(&mut p, "CopyFileW", &[source_w, copy_w, 1]), 1);
        assert_eq!(p.vfs.get(r"C:\Game\copyw.bin").unwrap(), b"file payload");
        p.wr32(TEB_ADDR + 0x34, 0).unwrap();
        assert_eq!(call_x86(&mut p, "CopyFileA", &[source_a, copy_a, 1]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), super::ERROR_FILE_EXISTS);

        let moved_w = (BASE + 0x200) as u32;
        p.write_wstr(source_w as u64, "copyw.bin").unwrap();
        p.write_wstr(moved_w as u64, "moved.bin").unwrap();
        assert_eq!(
            call_x86(&mut p, "MoveFileExW", &[source_w, moved_w, 0x1]),
            1
        );
        assert!(p.vfs.get(r"C:\Game\copyw.bin").is_none());
        assert_eq!(p.vfs.get(r"C:\Game\moved.bin").unwrap(), b"file payload");

        p.vfs.insert(r"C:\Game\move-a.bin", b"move".to_vec());
        let source_w = (BASE + 0x240) as u32;
        let moved_w = (BASE + 0x280) as u32;
        p.write_wstr(source_w as u64, "move-a.bin").unwrap();
        p.write_wstr(moved_w as u64, "move-b.bin").unwrap();
        assert_eq!(call_x86(&mut p, "MoveFileW", &[source_w, moved_w]), 1);
        assert!(p.vfs.get(r"C:\Game\move-a.bin").is_none());
        assert_eq!(p.vfs.get(r"C:\Game\move-b.bin").unwrap(), b"move");
    }

    #[test]
    fn set_file_time_updates_only_the_supplied_metadata_fields() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let path = r"C:\Game\metadata.bin";
        p.vfs.insert(path, b"metadata".to_vec());
        let handle = p.new_handle(Handle::File {
            path: path.into(),
            pos: 0,
            write: true,
            overlapped: false,
        });
        let creation = (BASE + 0x100) as u32;
        let access = (BASE + 0x108) as u32;
        p.wr64(creation as u64, 0x0123_4567_89ab_cdef).unwrap();
        p.wr64(access as u64, 0x1020_3040_5060_7080).unwrap();

        assert_eq!(
            call_x86(&mut p, "SetFileTime", &[handle as u32, creation, access, 0]),
            1
        );
        let status = p.vfs.file_status(path).unwrap();
        assert_eq!(status.created, Some(0x0123_4567_89ab_cdefu64 as i64));
        assert_eq!(status.accessed, Some(0x1020_3040_5060_7080u64 as i64));
        assert_eq!(status.modified, None);
        p.wr64(creation as u64, 0).unwrap();
        p.wr64(access as u64, u64::MAX).unwrap();
        assert_eq!(
            call_x86(&mut p, "SetFileTime", &[handle as u32, creation, access, 0]),
            1
        );
        let status = p.vfs.file_status(path).unwrap();
        assert_eq!(status.created, Some(0x0123_4567_89ab_cdefu64 as i64));
        assert_eq!(status.accessed, Some(0x1020_3040_5060_7080u64 as i64));
    }

    #[test]
    fn cancel_io_and_format_message_report_only_synchronous_supported_operations() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let file = p.new_handle(Handle::File {
            path: r"C:\Game\sync.bin".into(),
            pos: 0,
            write: false,
            overlapped: true,
        });
        assert_eq!(call_x86(&mut p, "CancelIo", &[file as u32]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), super::ERROR_NOT_FOUND);
        assert_eq!(call_x86(&mut p, "CancelIo", &[0xdead_beef]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);

        let source = (BASE + 0x100) as u32;
        let output = (BASE + 0x200) as u32;
        p.write_bytes(source as u64, b"hello%nworld!\0").unwrap();
        let count = call_x86(
            &mut p,
            "FormatMessageA",
            &[0x0000_0400, source, 0, 0, output, 32, 0],
        );
        assert_eq!(count, 13);
        assert_eq!(p.read_cstr(output as u64).unwrap(), "hello\r\nworld!");

        let system_count = call_x86(
            &mut p,
            "FormatMessageA",
            &[0x0000_1200, 0, 87, 0x0409, output, 64, 0],
        );
        assert_eq!(system_count, 29);
        assert_eq!(
            p.read_cstr(output as u64).unwrap(),
            "The parameter is incorrect.\r\n"
        );
        p.write_bytes(source as u64, b"item %1\0").unwrap();
        assert!(matches!(
            invoke_x86(
                &mut p,
                "FormatMessageA",
                &[0x0000_0400, source, 0, 0, output, 32, 0]
            ),
            Err(Stop::Unimplemented { .. })
        ));
        assert_eq!(
            call_x86(
                &mut p,
                "FormatMessageA",
                &[0x0000_0600, source, 0, 0, output, 32, 0]
            ),
            7
        );
        assert_eq!(p.read_cstr(output as u64).unwrap(), "item %1");

        let output_ptr = (BASE + 0x240) as u32;
        p.write_bytes(source as u64, b"hello%nworld!\0").unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "FormatMessageA",
                &[0x0000_0500, (BASE + 0x100) as u32, 0, 0, output_ptr, 0, 0]
            ),
            13
        );
        let allocated = p.rd32(output_ptr as u64).unwrap();
        assert_eq!(p.read_cstr(allocated as u64).unwrap(), "hello\r\nworld!");
    }

    #[test]
    fn set_thread_execution_state_clears_previous_requirements_and_returns_them() {
        const ES_CONTINUOUS: u32 = 0x8000_0000;
        let mut p = process();
        let thread_id = p.current_thread_id;
        p.extra
            .thread_execution_states
            .insert(thread_id, ES_CONTINUOUS | 0x3);

        assert_eq!(
            call_x86(&mut p, "SetThreadExecutionState", &[ES_CONTINUOUS]),
            (ES_CONTINUOUS | 0x3) as u64
        );
        assert_eq!(
            p.extra.thread_execution_states.get(&thread_id),
            Some(&ES_CONTINUOUS)
        );
        assert_eq!(
            call_x86(&mut p, "SetThreadExecutionState", &[ES_CONTINUOUS]),
            ES_CONTINUOUS as u64
        );
    }

    #[test]
    fn set_thread_execution_state_exposes_host_power_requests() {
        let mut p = process();
        for flags in [0, 1, 2, 0x8000_0001, 0x8000_0002, 0x8000_0040] {
            assert!(matches!(
                invoke_x86(&mut p, "SetThreadExecutionState", &[flags]),
                Err(Stop::Crash(message)) if message.contains("untranslated: kernel32!SetThreadExecutionState")
            ));
        }
    }

    fn invoke_x86(p: &mut Process, name: &str, args: &[u32]) -> Result<u64, crate::process::Stop> {
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap(); // return address
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        Ok(lookup(name).unwrap_or_else(|| {
            panic!("kernel32!{name} should resolve")
        })(p)?)
    }

    fn call_x86(p: &mut Process, name: &str, args: &[u32]) -> u64 {
        invoke_x86(p, name, args).unwrap()
    }

    fn add_resource_module(p: &mut Process, with_named_resource: bool) {
        const MODULE: u64 = 0x0040_0000;
        const RESOURCE_RVA: u32 = 0x1000;
        const RESOURCE_SIZE: u32 = 0x1000;
        p.mem.map(MODULE, 0x4000, PROT_RW);
        p.modules.push(crate::process::Module {
            name: "test.exe".into(),
            path: "test.exe".into(),
            base: MODULE,
            size: 0x4000,
            entry: 0,
            is_dll: false,
            tls_callbacks: Vec::new(),
            exports: Default::default(),
            ordinals: Default::default(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: RESOURCE_RVA,
            resource_size: RESOURCE_SIZE,
            hle_group: None,
        });

        let root = MODULE + RESOURCE_RVA as u64;
        p.wr16(root + 14, 1).unwrap();
        p.wr32(root + 16, 14).unwrap();
        p.wr32(root + 20, 0x8000_0020).unwrap();

        let type_dir = root + 0x20;
        let named_count = u16::from(with_named_resource);
        p.wr16(type_dir + 12, named_count).unwrap();
        p.wr16(type_dir + 14, 2).unwrap();
        let entries = type_dir + 16;
        let first_id_entry = if with_named_resource {
            p.wr32(entries, 0x8000_0300).unwrap();
            p.wr32(entries + 4, 0x8000_0080).unwrap();
            let name = root + 0x300;
            p.wr16(name, 5).unwrap();
            for (index, unit) in "Group".encode_utf16().enumerate() {
                p.wr16(name + 2 + index as u64 * 2, unit).unwrap();
            }
            entries + 8
        } else {
            entries
        };
        for (index, id) in [101u32, 102].into_iter().enumerate() {
            let entry = first_id_entry + index as u64 * 8;
            let lang_dir_offset = 0x80 + (index as u32 + u32::from(with_named_resource)) * 0x20;
            p.wr32(entry, id).unwrap();
            p.wr32(entry + 4, 0x8000_0000 | lang_dir_offset).unwrap();
        }

        // Each name leads through one language directory to a valid data entry.
        let lang_count = 2 + u32::from(with_named_resource);
        for index in 0..lang_count {
            let offset = 0x80 + index * 0x20;
            let directory = root + offset as u64;
            p.wr16(directory + 14, 1).unwrap();
            p.wr32(directory + 16, 0x0409).unwrap();
            p.wr32(directory + 20, 0x100 + index * 16).unwrap();
            let data = root + 0x100 + index as u64 * 16;
            p.wr32(data, 0x2000 + index * 0x20).unwrap();
            p.wr32(data + 4, 16).unwrap();
        }
    }

    fn enum_name_callback(p: &mut Process, code: u32, capture: u32, count: u32) -> u32 {
        let mut bytes = Vec::new();
        for (stack_offset, output_offset) in [(4u8, 0u32), (8, 4), (12, 8), (16, 12)] {
            bytes.extend_from_slice(&[0x8b, 0x44, 0x24, stack_offset]); // mov eax,[esp+offset]
            bytes.push(0xa3); // mov [absolute],eax
            bytes.extend_from_slice(&capture.wrapping_add(output_offset).to_le_bytes());
        }
        bytes.extend_from_slice(&[0xff, 0x05]); // inc dword ptr [absolute]
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.push(0xb8); // mov eax,return_value
        bytes.extend_from_slice(&code.to_le_bytes());
        bytes.extend_from_slice(&[0xc2, 0x10, 0x00]); // ret 16 (CALLBACK/stdcall)
        let address = (BASE + 0x300) as u32;
        p.write_bytes(address as u64, &bytes).unwrap();
        assert!(p.mem.protect(BASE, PAGE_SIZE, PROT_RWX));
        address
    }

    fn enum_named_resource_callback(p: &mut Process, capture: u32) -> u32 {
        let mut bytes = Vec::new();
        for (stack_offset, output_offset) in [(4u8, 0u32), (8, 4), (12, 8), (16, 12)] {
            bytes.extend_from_slice(&[0x8b, 0x44, 0x24, stack_offset]);
            bytes.push(0xa3);
            bytes.extend_from_slice(&capture.wrapping_add(output_offset).to_le_bytes());
        }
        bytes.extend_from_slice(&[0x8b, 0x4c, 0x24, 0x0c]); // ecx = lpName
        bytes.extend_from_slice(&[0x8a, 0x01]); // al = first byte of callback-scoped name
        bytes.push(0xa2); // mov [absolute],al
        bytes.extend_from_slice(&capture.wrapping_add(16).to_le_bytes());
        bytes.extend_from_slice(&[0xb8, 0, 0, 0, 0, 0xc2, 0x10, 0]); // stop after first name
        let address = (BASE + 0x300) as u32;
        p.write_bytes(address as u64, &bytes).unwrap();
        assert!(p.mem.protect(BASE, PAGE_SIZE, PROT_RWX));
        address
    }

    #[test]
    fn enum_resource_names_a_passes_resource_ids_and_stops_on_false() {
        let mut p = process();
        add_resource_module(&mut p, false);
        let capture = (BASE + 0x500) as u32;
        let count = capture + 0x20;
        let callback = enum_name_callback(&mut p, 0, capture, count);
        let result = invoke_x86(
            &mut p,
            "EnumResourceNamesA",
            &[0x0040_0000, 14, callback, 0x1234_5678],
        );
        assert_eq!(result.unwrap(), 0);
        assert_eq!(p.rd32(count as u64).unwrap(), 1);
        assert_eq!(p.rd32(capture as u64).unwrap(), 0x0040_0000);
        assert_eq!(p.rd32(capture as u64 + 4).unwrap(), 14);
        assert_eq!(p.rd32(capture as u64 + 8).unwrap(), 101);
        assert_eq!(p.rd32(capture as u64 + 12).unwrap(), 0x1234_5678);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 15106);
    }

    #[test]
    fn enum_resource_names_w_continues_through_all_matching_resource_ids() {
        let mut p = process();
        add_resource_module(&mut p, false);
        let capture = (BASE + 0x500) as u32;
        let count = capture + 0x20;
        let callback = enum_name_callback(&mut p, 1, capture, count);
        assert_eq!(
            call_x86(
                &mut p,
                "EnumResourceNamesW",
                &[0x0040_0000, 14, callback, 0x7654_3210],
            ),
            1
        );
        assert_eq!(p.rd32(count as u64).unwrap(), 2);
        assert_eq!(p.rd32(capture as u64 + 8).unwrap(), 102);
        assert_eq!(p.rd32(capture as u64 + 12).unwrap(), 0x7654_3210);
    }

    #[test]
    fn enum_resource_names_a_parses_numeric_type_strings_and_scopes_named_pointers() {
        let mut p = process();
        add_resource_module(&mut p, true);
        let type_string = (BASE + 0x100) as u32;
        let capture = (BASE + 0x500) as u32;
        p.write_cstr(type_string as u64, "#14").unwrap();
        let callback = enum_named_resource_callback(&mut p, capture);
        assert_eq!(
            call_x86(
                &mut p,
                "EnumResourceNamesA",
                &[0x0040_0000, type_string, callback, 0x1122_3344],
            ),
            0
        );
        assert_eq!(p.rd32(capture as u64).unwrap(), 0x0040_0000);
        assert_eq!(p.rd32(capture as u64 + 4).unwrap(), type_string);
        let name_pointer = p.rd32(capture as u64 + 8).unwrap();
        assert!(name_pointer > 0xffff);
        assert_eq!(p.rd32(capture as u64 + 12).unwrap(), 0x1122_3344);
        assert_eq!(p.rd8(capture as u64 + 16).unwrap(), b'G');
        assert!(p.heap.size_of(name_pointer as u64).is_none());
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 15106);
    }

    #[test]
    fn enum_resource_names_w_exposes_a_callback_scoped_utf16_name() {
        let mut p = process();
        add_resource_module(&mut p, true);
        let capture = (BASE + 0x500) as u32;
        let callback = enum_named_resource_callback(&mut p, capture);
        assert_eq!(
            call_x86(
                &mut p,
                "EnumResourceNamesW",
                &[0x0040_0000, 14, callback, 0x5566_7788],
            ),
            0
        );
        let name_pointer = p.rd32(capture as u64 + 8).unwrap();
        assert!(name_pointer > 0xffff);
        assert_eq!(p.rd8(capture as u64 + 16).unwrap(), b'G');
        assert!(p.heap.size_of(name_pointer as u64).is_none());
    }

    #[test]
    fn enum_resource_names_reports_missing_types_and_invalid_callbacks() {
        let mut p = process();
        add_resource_module(&mut p, false);
        let capture = (BASE + 0x500) as u32;
        let count = capture + 0x20;
        let callback = enum_name_callback(&mut p, 1, capture, count);
        assert_eq!(
            call_x86(
                &mut p,
                "EnumResourceNamesA",
                &[0x0040_0000, 10, callback, 0]
            ),
            0
        );
        assert_eq!(p.rd32(count as u64).unwrap(), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 1813);
        assert_eq!(
            call_x86(&mut p, "EnumResourceNamesA", &[0x0040_0000, 14, 0, 0]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn get_locale_info_w_returns_default_ansi_code_page_string() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoW", &[0x0409, 0x1004, output, 0x80]),
            5
        );
        assert_eq!(p.read_wstr(output as u64).unwrap(), "1252");
    }

    #[test]
    fn get_string_type_a_uses_its_five_argument_abi_and_windows_ctype1_data() {
        let mut p = process();
        let source = (BASE + 0x100) as u32;
        let output = source + 0x100;
        p.write_bytes(source as u64, b"A9 ").unwrap();

        assert_eq!(
            call_x86(&mut p, "GetStringTypeA", &[0x0409, 1, source, 3, output]),
            1
        );
        assert_eq!(p.rd16(output as u64).unwrap(), 0x0381); // C1_DEFINED | UPPER | XDIGIT | ALPHA
        assert_eq!(p.rd16(output as u64 + 2).unwrap(), 0x0284); // C1_DEFINED | DIGIT | XDIGIT
        assert_eq!(p.rd16(output as u64 + 4).unwrap(), 0x0248); // C1_DEFINED | SPACE | BLANK
    }

    #[test]
    fn get_string_type_w_includes_the_null_terminator_for_negative_length() {
        let mut p = process();
        let source = (BASE + 0x100) as u32;
        let output = source + 0x100;
        p.write_wstr(source as u64, "A").unwrap();

        assert_eq!(
            call_x86(&mut p, "GetStringTypeW", &[1, source, u32::MAX, output],),
            1
        );
        assert_eq!(p.rd16(output as u64).unwrap(), 0x0381);
        assert_eq!(p.rd16(output as u64 + 2).unwrap(), 0x0220); // C1_DEFINED | C1_CNTRL for U+0000
    }

    #[test]
    fn get_string_type_w_exposes_valid_ctype_levels_outside_the_translation() {
        let mut p = process();
        let source = (BASE + 0x100) as u32;
        let output = source + 0x100;
        p.write_wstr(source as u64, "A").unwrap();

        assert!(matches!(
            invoke_x86(&mut p, "GetStringTypeW", &[2, source, 1, output]),
            Err(Stop::Crash(message)) if message.contains("untranslated: kernel32!GetStringTypeW")
        ));
        assert!(matches!(
            invoke_x86(&mut p, "GetStringTypeW", &[1, source, 1, output]),
            Ok(1)
        ));
        p.write_wstr(source as u64, "é").unwrap();
        assert_eq!(
            call_x86(&mut p, "GetStringTypeW", &[1, source, 1, output]),
            1
        );
        assert_eq!(p.rd16(output as u64).unwrap(), 0x0302); // C1_DEFINED | C1_ALPHA | C1_LOWER
    }

    #[test]
    fn get_string_type_w_matches_windows_nls_across_unicode_and_surrogate_units() {
        let mut p = process();
        let source = (BASE + 0x100) as u32;
        let output = source + 0x100;
        let units = [
            0x20ac, 0x0301, 0xd83e, 0xdd73, 0x0378, 0x00a0, 0x0009, 0x00b2,
        ];
        for (index, unit) in units.into_iter().enumerate() {
            p.wr16(source as u64 + index as u64 * 2, unit).unwrap();
        }

        assert_eq!(
            call_x86(
                &mut p,
                "GetStringTypeW",
                &[1, source, units.len() as u32, output],
            ),
            1
        );
        assert_eq!(p.rd16(output as u64).unwrap(), 0x0200); // Euro: C1_DEFINED
        assert_eq!(p.rd16(output as u64 + 2).unwrap(), 0x0200); // combining mark: C1_DEFINED
        assert_eq!(p.rd16(output as u64 + 4).unwrap(), 0x0200); // high surrogate: C1_DEFINED
        assert_eq!(p.rd16(output as u64 + 6).unwrap(), 0x0200); // low surrogate: C1_DEFINED
        assert_eq!(p.rd16(output as u64 + 8).unwrap(), 0); // unassigned U+0378
        assert_eq!(p.rd16(output as u64 + 10).unwrap(), 0x0248); // NBSP: DEFINED | SPACE | BLANK
        assert_eq!(p.rd16(output as u64 + 12).unwrap(), 0x0268); // tab: DEFINED | CNTRL | SPACE | BLANK
        assert_eq!(p.rd16(output as u64 + 14).unwrap(), 0x0214); // superscript two: DEFINED | PUNCT
    }

    #[test]
    fn get_string_type_reports_invalid_flags_and_untranslated_ansi_profiles() {
        let mut p = process();
        let source = (BASE + 0x100) as u32;
        let output = source + 0x100;
        p.write_bytes(source as u64, b"A").unwrap();
        p.wr16(output as u64, 0xaaaa).unwrap();

        assert_eq!(
            call_x86(&mut p, "GetStringTypeA", &[0x0409, 4, source, 1, output]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), super::ERROR_INVALID_FLAGS);
        assert_eq!(p.rd16(output as u64).unwrap(), 0xaaaa);

        assert!(matches!(
            invoke_x86(&mut p, "GetStringTypeA", &[0x1004, 1, source, 1, output]),
            Err(Stop::Crash(message)) if message.contains("untranslated: kernel32!GetStringTypeA")
        ));
    }

    #[test]
    fn get_locale_info_a_queries_size_and_honors_insufficient_buffer() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        p.wr32(output as u64, 0xaabb_ccdd).unwrap();
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoA", &[0x0409, 0x1004, 0, 0]),
            5
        );
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoA", &[0x0409, 0x1004, output, 4]),
            0
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 0xaabb_ccdd);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 122); // ERROR_INSUFFICIENT_BUFFER
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoA", &[0x0409, 0x1004, output, 5]),
            5
        );
        assert_eq!(p.read_cstr(output as u64).unwrap(), "1252");
    }

    #[test]
    fn get_locale_info_return_number_writes_a_dword_and_reports_wchar_size() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoW", &[0x0409, 0x2000_1004, output, 2],),
            2
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 1252);
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoA", &[0x0409, 0x2000_1004, output, 4],),
            4
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 1252);
    }

    #[test]
    fn get_locale_info_supports_default_and_invariant_locale_ids() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        for locale in [0x0400, 0x0800, 0x007f, 0x0409] {
            assert_eq!(
                call_x86(&mut p, "GetLocaleInfoW", &[locale, 0x1004, output, 5]),
                5,
                "LCID {locale:#x}"
            );
            assert_eq!(p.read_wstr(output as u64).unwrap(), "1252");
        }
        // 0x1004 is zh-SG when it is the LCID, while it is the ANSI-code-page
        // LCTYPE when it is the second argument.
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoW", &[0x1004, 0x1004, output, 5]),
            4
        );
        assert_eq!(p.read_wstr(output as u64).unwrap(), "936");
    }

    #[test]
    fn get_locale_info_accepts_documented_combining_flags() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoA", &[0x0409, 0xc000_1004, output, 5],),
            5
        );
        assert_eq!(p.read_cstr(output as u64).unwrap(), "1252");
    }

    #[test]
    fn get_locale_info_reports_invalid_flags_and_malformed_lcids() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoW", &[0x0409, 0, output, 8]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 1004); // ERROR_INVALID_FLAGS
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoW", &[0x0409, 0x1000_1004, output, 8],),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 1004); // ERROR_INVALID_FLAGS
        assert_eq!(
            call_x86(&mut p, "GetLocaleInfoW", &[0xdead_beef, 0x1004, output, 8],),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
        assert_eq!(
            call_x86(
                &mut p,
                "GetLocaleInfoW",
                &[0x0409, 0x1004, output, u32::MAX]
            ),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn get_locale_info_stops_for_valid_but_untranslated_lctype_and_locale() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        for (locale, lctype) in [(0x0409, 0x0001), (0x0411, 0x1004)] {
            let result = invoke_x86(&mut p, "GetLocaleInfoW", &[locale, lctype, output, 8]);
            assert!(matches!(
                result,
                Err(crate::process::Stop::Crash(message))
                    if message.contains("untranslated") && message.contains("GetLocaleInfoW")
            ));
        }
    }

    #[test]
    fn get_locale_info_does_not_claim_untranslated_locale_data() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        let result = invoke_x86(&mut p, "GetLocaleInfoW", &[0x0409, 0x0000_0001, output, 8]);
        assert!(matches!(
            result,
            Err(crate::process::Stop::Crash(message))
                if message.contains("untranslated") && message.contains("GetLocaleInfoW")
        ));
    }

    #[test]
    fn set_thread_ideal_processor_updates_and_queries_per_thread_preference() {
        let mut p = process();
        assert_eq!(
            call_x86(&mut p, "SetThreadIdealProcessor", &[u32::MAX - 1, 0]),
            0
        ); // previous ideal processor for the current thread

        let handle = p.new_handle(Handle::Thread {
            id: 0x2000,
            start: 0,
            parameter: 0,
            exit_code: None,
            suspended: false,
            inheritable: false,
        });
        assert_eq!(
            call_x86(&mut p, "SetThreadIdealProcessor", &[handle as u32, 32]),
            0
        ); // MAXIMUM_PROCESSORS queries without changing
    }

    #[test]
    fn set_thread_ideal_processor_reports_invalid_handles_and_processor_numbers() {
        let mut p = process();
        assert_eq!(
            call_x86(&mut p, "SetThreadIdealProcessor", &[0xdead_beef, 0]),
            u32::MAX as u64
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);

        assert_eq!(
            call_x86(&mut p, "SetThreadIdealProcessor", &[u32::MAX - 1, 1]),
            u32::MAX as u64
        ); // only processor 0 is present in this guest profile
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn open_thread_suspend_and_resume_preserve_win32_suspend_counts() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let id = 0x2000;
        p.create_guest_thread(id, 0x3000, 0x44, false).unwrap();

        let handle = call_x86(&mut p, "OpenThread", &[0x001f_03ff, 1, id as u32]) as u32;
        assert_ne!(handle, 0);
        assert!(matches!(
            p.handles.get(&(handle as u64)),
            Some(Handle::Thread { id: handle_id, inheritable: true, .. }) if *handle_id == id
        ));

        assert_eq!(call_x86(&mut p, "SuspendThread", &[handle]), 0);
        assert_eq!(call_x86(&mut p, "SuspendThread", &[handle]), 1);
        assert_eq!(p.thread_suspend_counts.get(&id), Some(&2));
        assert_eq!(call_x86(&mut p, "ResumeThread", &[handle]), 2);
        assert_eq!(p.thread_suspend_counts.get(&id), Some(&1));
        assert_eq!(call_x86(&mut p, "ResumeThread", &[handle]), 1);
        assert_eq!(p.thread_suspend_counts.get(&id), None);
        assert_eq!(
            p.guest_threads.get(&id).map(|thread| &thread.state),
            Some(&crate::process::ThreadState::Ready)
        );
        assert_eq!(call_x86(&mut p, "ResumeThread", &[handle]), 0);

        assert_eq!(call_x86(&mut p, "OpenThread", &[0, 0, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn terminate_thread_and_get_exit_code_process_report_real_process_state() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        let id = 0x2000;
        p.create_guest_thread(id, 0x3000, 0, false).unwrap();
        let handle = call_x86(&mut p, "OpenThread", &[0x001f_03ff, 0, id as u32]) as u32;
        let output = (BASE + 0x100) as u32;

        assert_eq!(call_x86(&mut p, "TerminateThread", &[handle, 0x1234]), 1);
        assert!(matches!(
            p.handles.get(&(handle as u64)),
            Some(Handle::Thread {
                exit_code: Some(0x1234),
                ..
            })
        ));
        assert_eq!(call_x86(&mut p, "GetExitCodeThread", &[handle, output]), 1);
        assert_eq!(p.rd32(output as u64).unwrap(), 0x1234);

        p.wr32(output as u64, 0).unwrap();
        assert_eq!(
            call_x86(&mut p, "GetExitCodeProcess", &[u32::MAX, output]),
            1
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 259);
        p.exit_code = Some(42);
        assert_eq!(
            call_x86(&mut p, "GetExitCodeProcess", &[u32::MAX, output]),
            1
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 42);

        p.wr32(TEB_ADDR + 0x34, 0).unwrap();
        assert_eq!(
            call_x86(&mut p, "GetExitCodeProcess", &[0xdead_beef, output]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
        assert_eq!(call_x86(&mut p, "GetExitCodeProcess", &[u32::MAX, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn get_product_info_reports_the_virtual_windows_product_for_supported_versions() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        for (major, minor, expected) in [(10, 0, 0x30), (6, 1, 0x30), (6, 0, 0x06)] {
            p.wr32(output as u64, u32::MAX).unwrap();
            assert_eq!(
                call_x86(&mut p, "GetProductInfo", &[major, minor, 0, 0, output],),
                1,
                "GetProductInfo({major}, {minor}, 0, 0, output)"
            );
            assert_eq!(p.rd32(output as u64).unwrap(), expected);
        }
    }

    #[test]
    fn get_product_info_rejects_unsupported_os_versions_and_null_output() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        p.wr32(output as u64, u32::MAX).unwrap();
        assert_eq!(call_x86(&mut p, "GetProductInfo", &[5, 0, 0, 0, output]), 0);
        assert_eq!(p.rd32(output as u64).unwrap(), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 0);

        p.wr32(TEB_ADDR + 0x34, 0x1234).unwrap();
        assert_eq!(call_x86(&mut p, "GetProductInfo", &[10, 0, 0, 0, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 0x1234);
    }

    #[test]
    fn is_wow64_process_reports_guest_bitness_and_rejects_invalid_arguments() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        p.wr32(output as u64, u32::MAX).unwrap();
        assert_eq!(call_x86(&mut p, "IsWow64Process", &[u32::MAX, output]), 1);
        assert_eq!(p.rd32(output as u64).unwrap(), 1);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 0);

        let duplicated_process = p.new_handle(Handle::Process {
            id: 0x1000,
            inheritable: false,
        });
        assert_eq!(
            call_x86(
                &mut p,
                "IsWow64Process",
                &[duplicated_process as u32, output]
            ),
            1
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 1);

        p.wr32(output as u64, 0xaabb_ccdd).unwrap();
        assert_eq!(call_x86(&mut p, "IsWow64Process", &[0x1234, output]), 0);
        assert_eq!(p.rd32(output as u64).unwrap(), 0xaabb_ccdd);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 6); // ERROR_INVALID_HANDLE

        p.wr32(TEB_ADDR + 0x34, 0).unwrap();
        assert_eq!(call_x86(&mut p, "IsWow64Process", &[u32::MAX, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);

        p.cpu.mode = CpuMode::X86_64;
        p.wr32(BASE + 0x110, u32::MAX).unwrap();
        assert_eq!(call(&mut p, "IsWow64Process", &[u64::MAX, BASE + 0x110]), 1);
        assert_eq!(p.rd32(BASE + 0x110).unwrap(), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x68).unwrap(), 0);
    }

    #[test]
    fn duplicate_handle_converts_the_current_thread_pseudo_handle_for_sims() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;

        assert_eq!(
            call_x86(
                &mut p,
                "DuplicateHandle",
                &[0xffff_ffff, 0xffff_fffe, 0xffff_ffff, output, 0, 1, 2],
            ),
            1
        );

        let duplicate = p.rd32(output as u64).unwrap() as u64;
        assert!(duplicate != 0 && duplicate != u32::MAX as u64 - 1);
        assert!(matches!(
            p.handles.get(&duplicate),
            Some(Handle::Thread {
                id: 0x1004,
                inheritable: true,
                ..
            })
        ));
        assert_eq!(call_x86(&mut p, "GetThreadId", &[duplicate as u32]), 0x1004);
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("kernel32", "DuplicateHandle"),
            Some(28)
        );

        p.exit_current_guest_thread(37);
        assert!(matches!(
            p.handles.get(&duplicate),
            Some(Handle::Thread {
                id: 0x1004,
                exit_code: Some(37),
                ..
            })
        ));

        assert_eq!(call_x86(&mut p, "CloseHandle", &[duplicate as u32]), 1);
        assert!(!p.handles.contains_key(&duplicate));
        assert_eq!(call_x86(&mut p, "GetThreadId", &[0xffff_fffe]), 0x1004);
    }

    #[test]
    fn duplicate_handle_converts_process_pseudo_handles_to_real_handles() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        assert_eq!(
            call_x86(
                &mut p,
                "DuplicateHandle",
                &[0xffff_ffff, 0xffff_ffff, 0xffff_ffff, output, 0, 1, 2],
            ),
            1
        );
        let duplicate = p.rd32(output as u64).unwrap();
        assert!(matches!(
            p.handles.get(&(duplicate as u64)),
            Some(Handle::Process {
                id: 0x1000,
                inheritable: true,
            })
        ));
        assert_eq!(call_x86(&mut p, "GetProcessId", &[duplicate]), 0x1000);
        assert_eq!(call_x86(&mut p, "GetProcessId", &[0xffff_ffff]), 0x1000);
        assert_eq!(call_x86(&mut p, "GetFileType", &[duplicate]), 0);
    }

    #[test]
    fn duplicate_handle_stops_on_object_types_without_shared_identity_semantics() {
        let mut p = process();
        let event = p.new_handle(Handle::Event {
            signaled: false,
            manual: false,
        });
        let output = (BASE + 0x100) as u32;
        p.wr32(output as u64, 0xaabb_ccdd).unwrap();

        let result = invoke_x86(
            &mut p,
            "DuplicateHandle",
            &[0xffff_ffff, event as u32, 0xffff_ffff, output, 0, 0, 2],
        );
        assert!(matches!(
            result,
            Err(crate::process::Stop::Crash(message)) if message.contains("untranslated") && message.contains("event")
        ));
        assert_eq!(p.rd32(output as u64).unwrap(), 0xaabb_ccdd);
        assert!(matches!(p.handles.get(&event), Some(Handle::Event { .. })));
    }

    #[test]
    fn duplicate_handle_rejects_unknown_option_bits_as_invalid_parameter() {
        let mut p = process();
        let output = (BASE + 0x100) as u32;
        p.wr32(output as u64, 0xaabb_ccdd).unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "DuplicateHandle",
                &[0xffff_ffff, 0xffff_fffe, 0xffff_ffff, output, 0, 0, 4],
            ),
            0
        );
        assert_eq!(p.rd32(output as u64).unwrap(), 0xaabb_ccdd);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    fn call(p: &mut Process, name: &str, args: &[u64]) -> u64 {
        let mut registers = [0; 4];
        for (index, value) in args.iter().take(4).enumerate() {
            registers[index] = *value;
        }
        p.cpu.gpr[RCX] = registers[0];
        p.cpu.gpr[RDX] = registers[1];
        p.cpu.gpr[R8] = registers[2];
        p.cpu.gpr[R9] = registers[3];
        for (index, value) in args.iter().enumerate().skip(4) {
            p.wr64(STACK + 8 + index as u64 * 8, *value).unwrap();
        }
        lookup(name).unwrap_or_else(|| panic!("kernel32!{name} should resolve"))(p).unwrap()
    }

    #[test]
    fn unmodeled_constant_success_imports_are_left_unresolved() {
        for name in [
            "HeapSetInformation",
            "HeapLock",
            "HeapUnlock",
            "HeapCompact",
            "HeapWalk",
            "VirtualLock",
            "VirtualUnlock",
            "SetThreadErrorMode",
            "SetConsoleMode",
            "SetConsoleTitleA",
            "SetConsoleTitleW",
            "SetConsoleCP",
            "SetConsoleOutputCP",
            "ReadConsoleA",
            "ReadConsoleW",
            "AllocConsole",
            "FreeConsole",
            "AttachConsole",
            "GetLocaleInfoEx",
            "FoldStringW",
            "FormatMessageW",
            "Beep",
            "GetThreadContext",
            "SetThreadContext",
            "SetThreadAffinityMask",
            "SetThreadDescription",
            "GetLogicalProcessorInformation",
            "GetProcessTimes",
            "GetThreadTimes",
            "SetHandleInformation",
            "GetHandleInformation",
            "GetSystemFirmwareTable",
            "SetThreadStackGuarantee",
        ] {
            assert!(
                lookup(name).is_none(),
                "{name} should remain visibly unsupported"
            );
        }
    }

    #[test]
    fn environment_mutation_and_environment_string_lifetime_are_real() {
        let mut p = process();
        p.write_cstr(BASE + 0x100, "ANYWASM_TEST").unwrap();
        p.write_cstr(BASE + 0x120, "42").unwrap();
        assert_eq!(
            call(
                &mut p,
                "SetEnvironmentVariableA",
                &[BASE + 0x100, BASE + 0x120]
            ),
            1
        );
        assert_eq!(
            call(
                &mut p,
                "GetEnvironmentVariableA",
                &[BASE + 0x100, BASE + 0x200, 8],
            ),
            2
        );
        assert_eq!(p.read_cstr(BASE + 0x200).unwrap(), "42");

        let env_block = call(&mut p, "GetEnvironmentStringsW", &[]);
        assert!(p.heap.size_of(env_block).is_some());
        assert_eq!(call(&mut p, "FreeEnvironmentStringsW", &[env_block]), 1);
        assert!(p.heap.size_of(env_block).is_none());

        assert_eq!(
            call(&mut p, "SetEnvironmentVariableA", &[BASE + 0x100, 0]),
            1
        );
        assert_eq!(
            call(
                &mut p,
                "GetEnvironmentVariableA",
                &[BASE + 0x100, BASE + 0x200, 8],
            ),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x68).unwrap(), 203); // ERROR_ENVVAR_NOT_FOUND
        assert_eq!(call(&mut p, "FreeEnvironmentStringsA", &[0xdead_beef]), 0);
    }

    #[test]
    fn process_clock_and_heap_queries_reflect_runtime_state() {
        let mut p = process();
        assert_eq!(call(&mut p, "GetProcessId", &[u64::MAX]), 0x1000);
        assert_eq!(call(&mut p, "GetProcessId", &[0xdead_beef]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x68).unwrap(), 6); // ERROR_INVALID_HANDLE

        assert_eq!(
            call(
                &mut p,
                "GetSystemTimeAdjustment",
                &[BASE + 0x100, BASE + 0x104, BASE + 0x108],
            ),
            1
        );
        assert_eq!(p.rd32(BASE + 0x100).unwrap(), 10_000);
        assert_eq!(p.rd32(BASE + 0x104).unwrap(), 10_000);
        assert_eq!(p.rd32(BASE + 0x108).unwrap(), 1);

        let heap = p.heap.process_heap;
        let block = call(&mut p, "HeapAlloc", &[heap, 0, 13]);
        assert_ne!(block, 0);
        assert_eq!(call(&mut p, "HeapValidate", &[heap, 0, block]), 1);
        assert_eq!(call(&mut p, "HeapValidate", &[heap, 0, 0xdead_beef]), 0);
        assert_eq!(call(&mut p, "HeapValidate", &[0x1234, 0, block]), 0);
        assert_eq!(call(&mut p, "HeapSize", &[heap, 0, block]), 16);
        assert_eq!(call(&mut p, "HeapFree", &[heap, 0, block]), 1);
        assert_eq!(call(&mut p, "HeapFree", &[heap, 0, block]), 0);
    }

    #[test]
    fn unsupported_console_reads_stay_unresolved_and_modes_match_virtual_handles() {
        let mut p = process();
        p.stdin.extend(b"abc");
        assert!(lookup("ReadConsoleA").is_none());
        assert!(lookup("ReadConsoleW").is_none());

        assert_eq!(
            call(&mut p, "GetConsoleMode", &[super::STD_OUT, BASE + 0x200]),
            1
        );
        assert_eq!(p.rd32(BASE + 0x200).unwrap(), 3);
        assert_eq!(call(&mut p, "GetConsoleMode", &[0x1234, BASE + 0x200]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x68).unwrap(), 6); // ERROR_INVALID_HANDLE
        assert_eq!(call(&mut p, "FlushConsoleInputBuffer", &[super::STD_IN]), 1);
        assert!(p.stdin.is_empty());
        assert_eq!(
            call(&mut p, "FlushConsoleInputBuffer", &[super::STD_OUT]),
            0
        );
    }

    #[test]
    fn file_flush_and_end_of_file_follow_virtual_file_state() {
        let mut p = process();
        p.vfs.insert("C:\\Game\\save.dat", b"abcdef".to_vec());
        p.handles.insert(
            0x400,
            Handle::File {
                path: "game\\save.dat".into(),
                pos: 3,
                write: true,
                overlapped: false,
            },
        );
        assert_eq!(call(&mut p, "SetEndOfFile", &[0x400]), 1);
        assert_eq!(p.vfs.get("game\\save.dat").unwrap(), b"abc");
        assert_eq!(call(&mut p, "FlushFileBuffers", &[0x400]), 1);
        assert_eq!(call(&mut p, "FlushFileBuffers", &[0x404]), 0);
    }

    #[test]
    fn lockfile_tracks_exact_byte_ranges_and_enforces_them_on_file_io() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.vfs.insert("data.bin", (0u8..32).collect());
        for handle in [0x400, 0x404] {
            p.handles.insert(
                handle,
                Handle::File {
                    path: "data.bin".into(),
                    pos: 4,
                    write: true,
                    overlapped: false,
                },
            );
        }

        assert_eq!(call_x86(&mut p, "LockFile", &[0x400, 4, 0, 4, 0]), 1);
        assert_eq!(call_x86(&mut p, "LockFile", &[0x404, 8, 0, 2, 0]), 1);
        assert_eq!(call_x86(&mut p, "LockFile", &[0x404, 6, 0, 1, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_LOCK_VIOLATION);

        // UnlockFile requires the exact locked range and the handle that acquired it.
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x400, 4, 0, 3, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_NOT_LOCKED);
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x404, 4, 0, 4, 0]), 0);
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x400, 4, 0, 4, 0]), 1);

        assert_eq!(call_x86(&mut p, "LockFile", &[0x400, 4, 0, 4, 0]), 1);
        let buffer = BASE as u32 + 0x100;
        let count = BASE as u32 + 0x180;
        assert_eq!(
            call_x86(&mut p, "ReadFile", &[0x404, buffer, 2, count, 0]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_LOCK_VIOLATION);
        assert_eq!(p.rd32(count as u64).unwrap(), 0);

        p.write_bytes(buffer as u64, b"XY").unwrap();
        assert_eq!(
            call_x86(&mut p, "WriteFile", &[0x404, buffer, 2, count, 0]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_LOCK_VIOLATION);
        assert_eq!(p.vfs.get("data.bin").unwrap()[4..6], [4, 5]);

        // Closing the locking handle releases its outstanding ranges.
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x404, 8, 0, 2, 0]), 1);
        assert_eq!(call_x86(&mut p, "CloseHandle", &[0x400]), 1);
        assert!(p.extra.file_range_locks.is_empty());
        assert_eq!(
            call_x86(&mut p, "ReadFile", &[0x404, buffer, 2, count, 0]),
            1
        );
        assert_eq!(p.read_bytes(buffer as u64, 2).unwrap(), [4, 5]);
    }

    #[test]
    fn lockfile_preserves_high_words_zero_byte_locks_and_access_errors() {
        let mut p = process();
        p.cpu.mode = CpuMode::X86_32;
        p.vfs.insert("data.bin", Vec::new());
        p.handles.insert(
            0x400,
            Handle::File {
                path: "data.bin".into(),
                pos: 0,
                write: true,
                overlapped: false,
            },
        );
        p.handles.insert(
            0x404,
            Handle::File {
                path: "data.bin".into(),
                pos: 0,
                write: false,
                overlapped: false,
            },
        );

        // Locks may extend beyond EOF; both offset and length use their full 64-bit pairs.
        assert_eq!(call_x86(&mut p, "LockFile", &[0x400, 7, 1, 3, 1]), 1);
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x400, 7, 1, 3, 1]), 1);

        // A zero-byte request protects the byte at its offset on Windows.
        assert_eq!(call_x86(&mut p, "LockFile", &[0x400, 100, 0, 0, 0]), 1);
        assert_eq!(call_x86(&mut p, "LockFile", &[0x404, 99, 0, 2, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_LOCK_VIOLATION);
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x400, 100, 0, 0, 0]), 1);

        p.extra.file_access.insert(0x404, 0);
        assert_eq!(call_x86(&mut p, "LockFile", &[0x404, 0, 0, 1, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_ACCESS_DENIED);

        assert_eq!(
            call_x86(&mut p, "LockFile", &[0x400, u32::MAX, u32::MAX, 1, 0]),
            0
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
        assert_eq!(call_x86(&mut p, "UnlockFile", &[0x400, 0, 0, 1, 0]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_NOT_LOCKED);
    }
}

#[cfg(test)]
mod change_notification_tests {
    use super::{lookup, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER};
    use crate::process::{Process, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x30000;
    const STACK: u64 = BASE + 0x800;
    const WAIT_OBJECT_0: u64 = 0;
    const WAIT_TIMEOUT: u64 = 0x102;
    const INVALID_HANDLE: u32 = u32::MAX;

    fn process() -> Process {
        let mut vfs = Vfs::new();
        vfs.insert(r"Game\Bin\seed.dat", vec![1, 2, 3]);
        let mut p = Process::new(vfs);
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn process_with_streamed_assets() -> Process {
        const ASSET_PATH: &[u8] = b"Game\\Bin\\seed.dat";
        let index_len = 2 + ASSET_PATH.len() + 16;
        let data_start = 24 + index_len;
        let mut package = b"AGP1".to_vec();
        package.extend(1u32.to_le_bytes());
        package.extend((index_len as u64).to_le_bytes());
        package.extend((data_start as u64 + 3).to_le_bytes());
        package.extend((ASSET_PATH.len() as u16).to_le_bytes());
        package.extend(ASSET_PATH);
        package.extend((data_start as u64).to_le_bytes());
        package.extend(3u64.to_le_bytes());
        package.extend([1, 2, 3]);

        let mut vfs = Vfs::new();
        vfs.register_asset_index(&package[..data_start]).unwrap();
        let mut p = Process::new(vfs);
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = STACK;
        p
    }

    fn wide(p: &mut Process, address: u64, value: &str) -> u32 {
        p.write_wstr(address, value).unwrap();
        address as u32
    }

    fn call(p: &mut Process, name: &str, args: &[u32]) -> u64 {
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(name).unwrap_or_else(|| panic!("kernel32!{name} should resolve"))(p).unwrap()
    }

    #[test]
    fn create_file_reports_creation_disposition_errors_and_preserves_open_always_data() {
        let mut p = process();
        p.vfs
            .insert(r"Game\Bin\cache.package", b"existing".to_vec());
        let existing = wide(&mut p, BASE + 0x100, r"C:\Game\Bin\cache.package");

        let open = call(
            &mut p,
            "CreateFileW",
            &[existing, 0x4000_0000, 1, 0, 4, 0, 0],
        );
        assert_ne!(open, u64::MAX);
        assert_eq!(
            p.rd32(TEB_ADDR + 0x34).unwrap(),
            super::ERROR_ALREADY_EXISTS
        );
        assert_eq!(p.vfs.get(r"game\bin\cache.package").unwrap(), b"existing");

        let create = call(
            &mut p,
            "CreateFileW",
            &[existing, 0x4000_0000, 1, 0, 2, 0, 0],
        );
        assert_ne!(create, u64::MAX);
        assert_eq!(
            p.rd32(TEB_ADDR + 0x34).unwrap(),
            super::ERROR_ALREADY_EXISTS
        );
        assert!(p.vfs.get(r"game\bin\cache.package").unwrap().is_empty());

        let missing = wide(&mut p, BASE + 0x180, r"C:\Game\Bin\new-cache.package");
        let created = call(
            &mut p,
            "CreateFileW",
            &[missing, 0x4000_0000, 1, 0, 4, 0, 0],
        );
        assert_ne!(created, u64::MAX);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 0);
        assert!(p.vfs.get(r"game\bin\new-cache.package").unwrap().is_empty());

        assert_eq!(
            call(
                &mut p,
                "CreateFileW",
                &[existing, 0x4000_0000, 1, 0, 1, 0, 0]
            ),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), super::ERROR_FILE_EXISTS);
    }

    fn watcher(p: &mut Process, subtree: u32, filter: u32) -> u32 {
        let path = wide(p, BASE + 0x100, r"C:\Game\Bin");
        call(p, "FindFirstChangeNotificationW", &[path, subtree, filter]) as u32
    }

    #[test]
    fn change_notification_is_waitable_one_shot_until_rearmed_and_tracks_guest_file_changes() {
        let mut p = process();
        let watch = watcher(&mut p, 1, 0x51); // FILE_NAME | LAST_WRITE | CREATION
        assert_ne!(watch, INVALID_HANDLE);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[watch, 0]),
            WAIT_TIMEOUT
        );

        let path = wide(&mut p, BASE + 0x180, r"C:\Game\Bin\new.dat");
        let file = call(&mut p, "CreateFileW", &[path, 0x4000_0000, 0, 0, 4, 0, 0]) as u32;
        assert_ne!(file, INVALID_HANDLE);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[watch, 0]),
            WAIT_OBJECT_0
        );
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[watch, 0]),
            WAIT_OBJECT_0
        );

        assert_eq!(call(&mut p, "FindNextChangeNotification", &[watch]), 1);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[watch, 0]),
            WAIT_TIMEOUT
        );

        let contents = (BASE + 0x280) as u32;
        p.write_bytes(contents as u64, b"changed").unwrap();
        let written = (BASE + 0x2c0) as u32;
        assert_eq!(
            call(&mut p, "WriteFile", &[file, contents, 7, written, 0]),
            1
        );
        assert_eq!(p.rd32(written as u64).unwrap(), 7);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[watch, 0]),
            WAIT_OBJECT_0
        );

        assert_eq!(call(&mut p, "FindCloseChangeNotification", &[watch]), 1);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[watch, 0]),
            u32::MAX as u64
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
    }

    #[test]
    fn streamed_change_notification_coverage_is_reported_once_per_process() {
        let mut p = process_with_streamed_assets();
        for _ in 0..3 {
            assert_ne!(watcher(&mut p, 1, 0x51), INVALID_HANDLE);
        }
        let warning_count = p
            .stderr
            .split(|byte| *byte == b'\n')
            .filter(|line| line.starts_with(b"[coverage] kernel32!FindFirstChangeNotificationW"))
            .count();
        assert_eq!(warning_count, 1);
    }

    #[test]
    fn change_notification_rejects_missing_directories_and_unobservable_filter_types() {
        let mut p = process();
        let missing = wide(&mut p, BASE + 0x100, r"C:\Game\Missing");
        assert_eq!(
            call(&mut p, "FindFirstChangeNotificationW", &[missing, 1, 0x51]),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 3);

        let path = wide(&mut p, BASE + 0x180, r"C:\Game\Bin");
        assert_eq!(
            call(&mut p, "FindFirstChangeNotificationW", &[path, 1, 0]),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);

        assert_eq!(
            call(&mut p, "FindFirstChangeNotificationW", &[path, 1, 0x100]),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 50); // security metadata is not modeled

        assert_eq!(
            call(&mut p, "FindFirstChangeNotificationW", &[path, 1, 0x20]),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 50); // access timestamps are not modeled

        assert_eq!(
            call(&mut p, "FindFirstChangeNotificationW", &[path, 1, 0x200]),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn change_notification_honors_subtree_masks_and_preserves_changes_until_rearm() {
        let mut p = process();
        p.vfs.insert(r"Game\Bin\sub\seed.dat", vec![1]);
        let direct = watcher(&mut p, 0, 0x01); // direct children, file names only
        let recursive = watcher(&mut p, 1, 0x01);

        let nested = wide(&mut p, BASE + 0x180, r"C:\Game\Bin\sub\nested.dat");
        let nested_file = call(&mut p, "CreateFileW", &[nested, 0x4000_0000, 0, 0, 4, 0, 0]);
        assert_ne!(nested_file, u64::MAX);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[direct, 0]),
            WAIT_TIMEOUT
        );
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[recursive, 0]),
            WAIT_OBJECT_0
        );

        let nested_again = wide(&mut p, BASE + 0x180, r"C:\Game\Bin\sub\again.dat");
        assert_ne!(
            call(
                &mut p,
                "CreateFileW",
                &[nested_again, 0x4000_0000, 0, 0, 4, 0, 0]
            ),
            u64::MAX
        );
        assert_eq!(call(&mut p, "FindNextChangeNotification", &[recursive]), 1);
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[recursive, 0]),
            WAIT_OBJECT_0
        );

        let direct_child = wide(&mut p, BASE + 0x180, r"C:\Game\Bin\direct.dat");
        assert_ne!(
            call(
                &mut p,
                "CreateFileW",
                &[direct_child, 0x4000_0000, 0, 0, 4, 0, 0]
            ),
            u64::MAX
        );
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[direct, 0]),
            WAIT_OBJECT_0
        );

        let last_write = watcher(&mut p, 1, 0x10);
        let size = watcher(&mut p, 1, 0x08);
        let seed = wide(&mut p, BASE + 0x180, r"C:\Game\Bin\seed.dat");
        let seed_file = call(&mut p, "CreateFileW", &[seed, 0x4000_0000, 0, 0, 3, 0, 0]) as u32;
        assert_ne!(seed_file, INVALID_HANDLE);
        p.write_bytes(BASE + 0x280, &[9, 8, 7]).unwrap();
        let written = (BASE + 0x2c0) as u32;
        assert_eq!(
            call(
                &mut p,
                "WriteFile",
                &[seed_file, (BASE + 0x280) as u32, 3, written, 0]
            ),
            1
        );
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[last_write, 0]),
            WAIT_OBJECT_0
        );
        assert_eq!(
            call(&mut p, "WaitForSingleObject", &[size, 0]),
            WAIT_TIMEOUT
        );
    }

    #[test]
    fn change_notification_rejects_files_and_reports_invalid_rearm_and_close_handles() {
        let mut p = process();
        let file_path = wide(&mut p, BASE + 0x100, r"C:\Game\Bin\seed.dat");
        assert_eq!(
            call(
                &mut p,
                "FindFirstChangeNotificationW",
                &[file_path, 1, 0x01]
            ),
            u64::MAX
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 3);

        assert_eq!(call(&mut p, "FindNextChangeNotification", &[0xdead]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
        assert_eq!(call(&mut p, "FindCloseChangeNotification", &[0xdead]), 0);
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
    }
}

#[cfg(test)]
mod find_file_directory_tests {
    use super::{find_first_file, find_next_file};
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const STACK: u64 = 0x80000;

    #[test]
    fn find_first_and_next_report_real_directory_attributes_and_file_sizes() {
        let mut vfs = Vfs::new();
        vfs.insert(
            r"GameData\Shared\Packages\FullBuild0.package",
            vec![1, 2, 3],
        );
        vfs.insert(r"GameData\Shared\Resource.cfg", vec![7]);
        vfs.set_root_alias(r"D:\The Sims 3").unwrap();

        let mut p = Process::new(vfs);
        assert!(p.mem.map(STACK, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK + 0x800;
        let search = crate::hle::kernel32::alloc_block(&mut p, 160, true);
        let output = crate::hle::kernel32::alloc_block(&mut p, 592, true);
        p.write_wstr(search, r"D:\The Sims 3\GameData\Shared\*")
            .unwrap();
        p.wr32(STACK + 0x804, search as u32).unwrap();
        p.wr32(STACK + 0x808, output as u32).unwrap();

        let handle = find_first_file(&mut p, true).unwrap();
        assert_ne!(handle as u32, u32::MAX);
        assert_eq!(p.rd32(output).unwrap(), 0x10); // FILE_ATTRIBUTE_DIRECTORY
        assert_eq!(p.read_wstr(output + 44).unwrap(), "packages");

        p.wr32(STACK + 0x804, handle as u32).unwrap();
        p.wr32(STACK + 0x808, output as u32).unwrap();
        assert_eq!(find_next_file(&mut p, true).unwrap(), 1);
        assert_eq!(p.rd32(output).unwrap(), 0x80); // FILE_ATTRIBUTE_NORMAL
        assert_eq!(p.read_wstr(output + 44).unwrap(), "resource.cfg");
        assert_eq!(p.rd32(output + 32).unwrap(), 0);
        assert_eq!(p.rd32(output + 36).unwrap(), 1);
    }
}

#[cfg(test)]
mod version_and_temp_path_tests {
    use super::lookup;
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x1_00000;
    const STACK: u64 = BASE + 0x800;
    const BUFFER: u64 = BASE + 0x100;

    fn process() -> Process {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.gpr[RSP] = STACK;
        process.wr32(STACK, 0).unwrap();
        process
    }

    fn call(process: &mut Process, name: &str, args: &[u32]) -> u64 {
        for (index, arg) in args.iter().enumerate() {
            process.wr32(STACK + 4 + index as u64 * 4, *arg).unwrap();
        }
        lookup(name).expect("kernel32 API should resolve")(process).unwrap()
    }

    #[test]
    fn get_version_ex_initializes_every_field_for_ansi_and_wide_base_and_ex_structs() {
        for (api, size, csd_bytes, ex) in [
            ("GetVersionExA", 148u32, 128u64, false),
            ("GetVersionExA", 156, 128, true),
            ("GetVersionExW", 276, 256, false),
            ("GetVersionExW", 284, 256, true),
        ] {
            let mut process = process();
            process.write_bytes(BUFFER, &[0xa5; 300]).unwrap();
            process.wr32(BUFFER, size).unwrap();

            assert_eq!(
                call(&mut process, api, &[BUFFER as u32]),
                1,
                "{api} size={size}"
            );
            assert_eq!(process.rd32(BUFFER).unwrap(), size);
            assert_eq!(process.rd32(BUFFER + 4).unwrap(), 10);
            assert_eq!(process.rd32(BUFFER + 8).unwrap(), 0);
            assert_eq!(process.rd32(BUFFER + 12).unwrap(), 19045);
            assert_eq!(process.rd32(BUFFER + 16).unwrap(), 2);
            assert_eq!(
                process.read_bytes(BUFFER + 20, csd_bytes as usize).unwrap(),
                vec![0; csd_bytes as usize],
                "{api} size={size} CSDVersion"
            );

            if ex {
                assert_eq!(
                    process.read_bytes(BUFFER + 20 + csd_bytes, 8).unwrap(),
                    [0, 0, 0, 0, 0, 0, 1, 0],
                    "{api} size={size} OSVERSIONINFOEX fields"
                );
            }
            assert!(process
                .read_bytes(BUFFER + size as u64, 4)
                .unwrap()
                .iter()
                .all(|byte| *byte == 0xa5));
        }
    }

    #[test]
    fn get_version_ex_rejects_invalid_struct_sizes_without_partial_output() {
        for api in ["GetVersionExA", "GetVersionExW"] {
            let mut process = process();
            process.write_bytes(BUFFER, &[0xa5; 300]).unwrap();
            process.wr32(BUFFER, 1).unwrap();
            let untouched = process.read_bytes(BUFFER + 4, 24).unwrap();

            assert_eq!(call(&mut process, api, &[BUFFER as u32]), 0, "{api}");
            assert_eq!(
                process.read_bytes(BUFFER + 4, 24).unwrap(),
                untouched,
                "{api}"
            );
        }
    }

    #[test]
    fn get_temp_path_w_writes_the_terminator_when_capacity_is_exact() {
        let mut process = process();
        process.write_bytes(BUFFER, &[0xa5; 24]).unwrap();

        assert_eq!(call(&mut process, "GetTempPathW", &[9, BUFFER as u32]), 8);
        assert_eq!(
            process.read_bytes(BUFFER, 18).unwrap(),
            [b'C', 0, b':', 0, b'\\', 0, b'T', 0, b'e', 0, b'm', 0, b'p', 0, b'\\', 0, 0, 0,]
        );
        assert_eq!(process.read_bytes(BUFFER + 18, 6).unwrap(), [0xa5; 6]);
    }

    #[test]
    fn get_temp_path_w_reports_nine_chars_needed_when_capacity_is_too_small() {
        let mut process = process();
        process.write_bytes(BUFFER, &[0xa5; 24]).unwrap();

        assert_eq!(call(&mut process, "GetTempPathW", &[8, BUFFER as u32]), 9);
    }
}

#[cfg(test)]
mod remove_directory_tests {
    use super::lookup;
    use crate::process::{Process, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x60000;
    const STACK: u64 = BASE + 0x800;
    const PATH: u64 = BASE + 0x100;

    fn process(vfs: Vfs) -> Process {
        let mut process = Process::new(vfs);
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.gpr[RSP] = STACK;
        process.wr32(STACK, 0).unwrap();
        process
    }

    fn remove_directory(process: &mut Process, path: &str) -> u64 {
        process.write_wstr(PATH, path).unwrap();
        process.wr32(STACK + 4, PATH as u32).unwrap();
        lookup("RemoveDirectoryW").expect("RemoveDirectoryW should resolve")(process).unwrap()
    }

    #[test]
    fn remove_directory_w_removes_only_empty_guest_directories() {
        let mut vfs = Vfs::new();
        vfs.create_dir("Game").unwrap();
        vfs.create_dir("Game\\empty").unwrap();
        vfs.create_dir("Game\\nonempty").unwrap();
        vfs.insert("Game\\nonempty\\save.dat", vec![1, 2, 3]);
        let mut process = process(vfs);

        assert_eq!(remove_directory(&mut process, r"C:\Game\empty"), 1);
        assert!(!process.vfs.is_dir(r"C:\Game\empty"));

        assert_eq!(remove_directory(&mut process, r"C:\Game\nonempty"), 0);
        assert_eq!(process.rd32(TEB_ADDR + 0x34).unwrap(), 145); // ERROR_DIR_NOT_EMPTY
        assert!(process.vfs.is_dir(r"C:\Game\nonempty"));
    }

    #[test]
    fn remove_directory_w_rejects_current_and_file_paths() {
        let mut vfs = Vfs::new();
        vfs.create_dir("Game").unwrap();
        vfs.create_dir("Game\\empty").unwrap();
        vfs.insert("Game\\save.dat", vec![1]);
        let mut process = process(vfs);

        assert_eq!(remove_directory(&mut process, "C:\\"), 0);
        assert_eq!(process.rd32(TEB_ADDR + 0x34).unwrap(), 5); // ERROR_ACCESS_DENIED
        assert_eq!(remove_directory(&mut process, r"C:\Game\save.dat"), 0);
        assert_eq!(process.rd32(TEB_ADDR + 0x34).unwrap(), 267); // ERROR_DIRECTORY
        assert!(process.vfs.exists(r"C:\Game\save.dat"));
    }
}

#[cfg(test)]
mod import_batch_tests {
    use super::lookup;
    use crate::process::{Process, Stop, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x70000;
    const STACK: u64 = BASE + 0x800;
    const OUTPUT: u64 = BASE + 0x100;

    fn process() -> Process {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.gpr[RSP] = STACK;
        process.wr32(STACK, 0).unwrap();
        process
    }

    fn invoke_x86(process: &mut Process, api: &str, args: &[u32]) -> Result<u64, Stop> {
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.gpr[RSP] = STACK;
        process.wr32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            process.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(api).unwrap_or_else(|| panic!("kernel32!{api} must be explicitly bound"))(process)
    }

    #[test]
    fn get_user_geo_id_reports_virtual_nation_and_fails_closed_for_unmodeled_region() {
        let mut process = process();
        assert_eq!(
            invoke_x86(&mut process, "GetUserGeoID", &[16]).unwrap(),
            244
        );
        assert_eq!(
            invoke_x86(&mut process, "GetUserGeoID", &[0]).unwrap(),
            u32::MAX as u64
        );
        assert!(matches!(
            invoke_x86(&mut process, "GetUserGeoID", &[14]),
            Err(Stop::Unimplemented { dll, name })
                if dll == "kernel32" && name.contains("GetUserGeoID") && name.contains("user-region GEOID")
        ));
    }

    #[test]
    fn get_geo_info_a_matches_us_windows_values_and_buffer_rules() {
        let mut process = process();
        for (geo_type, value) in [
            (1, "244"),
            (2, "39.450"),
            (3, "-98.908"),
            (4, "US"),
            (5, "USA"),
            (6, "en-us"),
            (7, "00000409"),
            (8, "United States"),
            (9, "United States of America"),
            (12, "840"),
            (14, "1"),
            (15, "USD"),
            (16, "$"),
            (17, "US"),
            (18, "244"),
        ] {
            let required = value.len() as u32 + 1;
            assert_eq!(
                invoke_x86(&mut process, "GetGeoInfoA", &[244, geo_type, 0, 0, 0]).unwrap(),
                required as u64,
                "GEO type {geo_type} size query"
            );
            assert_eq!(
                invoke_x86(
                    &mut process,
                    "GetGeoInfoA",
                    &[244, geo_type, OUTPUT as u32, required, 0x0409]
                )
                .unwrap(),
                required as u64,
                "GEO type {geo_type} output"
            );
            assert_eq!(
                process.read_cstr(OUTPUT).unwrap(),
                value,
                "GEO type {geo_type}"
            );
        }

        process.write_bytes(OUTPUT, &[0xa5; 8]).unwrap();
        assert_eq!(
            invoke_x86(&mut process, "GetGeoInfoA", &[244, 4, OUTPUT as u32, 2, 0]).unwrap(),
            0
        );
        assert_eq!(process.rd32(TEB_ADDR + 0x34).unwrap(), 122); // ERROR_INSUFFICIENT_BUFFER
        assert_eq!(process.read_bytes(OUTPUT, 8).unwrap(), [0xa5; 8]);

        assert_eq!(
            invoke_x86(&mut process, "GetGeoInfoA", &[0xdead_beef, 4, 0, 0, 0]).unwrap(),
            0
        );
        assert_eq!(process.rd32(TEB_ADDR + 0x34).unwrap(), 87); // ERROR_INVALID_PARAMETER
        assert_eq!(
            invoke_x86(&mut process, "GetGeoInfoA", &[244, 99, 0, 0, 0]).unwrap(),
            0
        );
        assert_eq!(process.rd32(TEB_ADDR + 0x34).unwrap(), 1004); // ERROR_INVALID_FLAGS
    }

    #[test]
    fn unsupported_kernel32_batch_is_bound_but_never_reports_fake_success() {
        for api in [
            "QueueUserAPC",
            "DisconnectNamedPipe",
            "ConnectNamedPipe",
            "CreateNamedPipeW",
            "CreateProcessW",
            "DeviceIoControl",
            "GetWriteWatch",
            "CreatePipe",
            "OpenMutexW",
            "OpenSemaphoreW",
            "OpenEventW",
        ] {
            let mut process = process();
            assert!(
                matches!(
                    invoke_x86(&mut process, api, &[]),
                    Err(Stop::Unimplemented { dll, name })
                        if dll == "kernel32" && name.contains(api)
                ),
                "{api} must fail closed with an explicit unsupported reason"
            );
        }
    }
}
