//! user32 subset: window classes, windows, message queue/pump, input state, plus winmm timers.
//!
//! Windows are virtual; the first top-level window is mirrored to the browser canvas through the
//! GPU command stream (`gpu::op::WINDOW`). Window procedures are real guest code, entered with
//! `Process::call_guest`. `GetMessage` with an empty queue returns `Stop::Idle` *without* advancing
//! the guest, so the host can push input and resume at the same instruction.

use super::gpu::{op, Pk};
use super::*;
use crate::process::*;
use anywasm_cpu::mem::PROT_RW;
use anywasm_cpu::CpuMode;
use std::collections::{HashMap, HashSet, VecDeque};

pub const WM_CREATE: u64 = 0x0001;
pub const WM_DESTROY: u64 = 0x0002;
pub const WM_SIZE: u64 = 0x0005;
pub const WM_SETFOCUS: u64 = 0x0007;
pub const WM_KILLFOCUS: u64 = 0x0008;
pub const WM_PAINT: u64 = 0x000f;
pub const WM_CLOSE: u64 = 0x0010;
pub const WM_QUIT: u64 = 0x0012;
pub const WM_ERASEBKGND: u64 = 0x0014;
pub const WM_SHOWWINDOW: u64 = 0x0018;
pub const WM_ACTIVATE: u64 = 0x0006;
pub const WM_NCCREATE: u64 = 0x0081;
pub const WM_NCDESTROY: u64 = 0x0082;
pub const WM_KEYDOWN: u64 = 0x0100;
pub const WM_KEYUP: u64 = 0x0101;
pub const WM_CHAR: u64 = 0x0102;
pub const WM_MOUSEMOVE: u64 = 0x0200;
pub const WM_LBUTTONDOWN: u64 = 0x0201;
pub const WM_LBUTTONUP: u64 = 0x0202;
pub const WM_RBUTTONDOWN: u64 = 0x0204;
pub const WM_RBUTTONUP: u64 = 0x0205;
pub const WM_MBUTTONDOWN: u64 = 0x0207;
pub const WM_MBUTTONUP: u64 = 0x0208;
pub const WM_MOUSEWHEEL: u64 = 0x020a;
pub const WM_MOUSELEAVE: u64 = 0x02a3;

const PM_REMOVE: u64 = 1;
const TME_LEAVE: u32 = 0x0000_0002;
const TME_CANCEL: u32 = 0x8000_0000;
const HWND_BASE: u64 = 0x0001_0010;
const WS_CHILD: u32 = 0x4000_0000;

fn set_windows_hook(p: &mut Process) -> R {
    let (hook_type, callback, module, thread_id) = (
        p.arg(0) as u32 as i32,
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32,
    );
    if callback == 0 || !(-1..=14).contains(&hook_type) {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let sequence = p.extra.win.next_hook.wrapping_add(1).max(1);
    p.extra.win.next_hook = sequence;
    let handle = 0x5000_0000 | sequence as u64;
    p.extra.win.hooks.insert(
        handle,
        Hook {
            hook_type,
            callback,
            module,
            thread_id,
        },
    );
    Ok(handle)
}

fn register_window_message(p: &mut Process, wide: bool) -> R {
    let ptr = p.arg(0);
    let name = if wide {
        p.read_wstr(ptr)?
    } else {
        p.read_cstr(ptr)?
    };
    if let Some(&id) = p.extra.win.registered_messages.get(&name) {
        return Ok(id as u64);
    }
    let id = p.extra.win.next_registered_message.max(0xc000);
    if id > 0xffff {
        p.set_last_error(8); // ERROR_NOT_ENOUGH_MEMORY
        return Ok(0);
    }
    p.extra.win.next_registered_message = id + 1;
    p.extra.win.registered_messages.insert(name, id);
    Ok(id as u64)
}

#[derive(Clone)]
pub struct Class {
    pub wndproc: u64,
    pub name: String,
    pub unicode: bool,
}

#[derive(Clone)]
pub struct Window {
    pub class: String,
    pub wndproc: u64,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub visible: bool,
    pub style: u32,
    pub user_data: u64,
    pub parent: u64,
    pub control_id: u32,
    pub unicode: bool,
    pub needs_paint: bool,
    pub gpu_id: u32,
}

#[derive(Clone, Copy)]
pub struct Msg {
    pub hwnd: u64,
    pub msg: u64,
    pub wparam: u64,
    pub lparam: u64,
    pub time: u32,
    pub pt: (i32, i32),
}

#[derive(Clone, Copy)]
pub struct Hook {
    pub hook_type: i32,
    pub callback: u64,
    pub module: u64,
    pub thread_id: u32,
}

#[derive(Default)]
pub struct WinState {
    pub classes: HashMap<String, Class>,
    pub atoms: HashMap<u16, String>,
    pub windows: HashMap<u64, Window>,
    pub queue: VecDeque<Msg>,
    pub quit: Option<i32>,
    next: u64,
    next_atom: u16,
    pub keys: Vec<u8>,
    pub cursor: (i32, i32),
    pub mouse_position_valid: bool,
    pub mouse_dx: i32,
    pub mouse_dy: i32,
    pub mouse_wheel: i32,
    pub mouse_tracking: HashSet<u64>,
    pub focus: u64,
    pub capture: u64,
    pub main_window: u64,
    pub timers: Vec<(u64, u64, u32, u64)>,
    pub props: HashMap<(u64, String), u64>,
    pub hooks: HashMap<u64, Hook>,
    next_hook: u32,
    pub registered_messages: HashMap<String, u32>,
    next_registered_message: u32,
}

fn lc(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Class names may be an atom (low word) instead of a pointer.
fn class_name(p: &Process, ptr: u64, wide: bool) -> Result<String, Stop> {
    if ptr < 0x10000 {
        return Ok(format!("#atom{ptr}"));
    }
    if wide { p.read_wstr(ptr) } else { p.read_cstr(ptr) }
}

pub fn lookup_winmm(name: &str) -> Option<HleFn> {
    Some(match name {
        "timeGetTime" => |p| Ok((p.cpu.icount + p.extra.sleep_adjust) / 100_000 & 0xffff_ffff),
        "timeBeginPeriod" | "timeEndPeriod" => |_| Ok(0),
        "timeGetDevCaps" => |p| {
            let a = p.arg(0);
            p.wr32(a, 1)?;
            p.wr32(a + 4, 1_000_000)?;
            Ok(0)
        },
        _ => return None,
    })
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "RegisterClassA" => |p| register_class(p, false, false),
        "RegisterClassW" => |p| register_class(p, true, false),
        "RegisterClassExA" => |p| register_class(p, false, true),
        "RegisterClassExW" => |p| register_class(p, true, true),
        "wsprintfA" => wsprintf_a,
        "CharUpperA" => |p| char_case_a(p, true, false),
        "CharLowerA" => |p| char_case_a(p, false, false),
        "CharUpperBuffA" => |p| char_case_a(p, true, true),
        "CharLowerBuffA" => |p| char_case_a(p, false, true),
        "UnregisterClassA" | "UnregisterClassW" => |_| Ok(1),
        "RegisterWindowMessageA" => |p| register_window_message(p, false),
        "RegisterWindowMessageW" => |p| register_window_message(p, true),
        "CreateWindowExA" => |p| create_window(p, false),
        "CreateWindowExW" => |p| create_window(p, true),
        "DestroyWindow" => |p| {
            let h = p.arg(0);
            destroy_window(p, h)?;
            Ok(1)
        },
        "ShowWindow" => |p| {
            let (h, cmd) = (p.arg(0), p.arg(1));
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.visible = cmd != 0;
                w.needs_paint = cmd != 0;
            }
            send(p, h, WM_SHOWWINDOW, (cmd != 0) as u64, 0)?;
            Ok(0)
        },
        "GetWindowPlacement" => get_window_placement,
        "GetKeyboardType" => |p| {
            Ok(match p.arg(0) {
                0 => 4, // enhanced 101/102-key keyboard
                1 => 0,
                2 => 12,
                _ => 0,
            })
        },
        "SetWindowPlacement" => set_window_placement,
        "UpdateWindow" => |p| {
            let h = p.arg(0);
            paint_now(p, h)?;
            Ok(1)
        },
        "RegisterDeviceNotificationW" => |p| {
            let (recipient, filter, flags) = (p.arg(0), p.arg(1), p.arg(2) as u32);
            if recipient == 0 || filter == 0 {
                p.set_last_error(87); // ERROR_INVALID_PARAMETER
                return Ok(0);
            }
            if flags & 1 == 0 && !p.extra.win.windows.contains_key(&recipient) {
                p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
                return Ok(0);
            }
            Ok(p.new_handle(Handle::Other))
        },
        "UnregisterDeviceNotification" => |p| {
            Ok(p.handles.remove(&p.arg(0)).is_some() as u64)
        },
        "EnumDisplayDevicesW" => enum_display_devices_w,
        "EnumDisplaySettingsW" => enum_display_settings_w,
        "EnumDisplayMonitors" => enum_display_monitors,
        "InvalidateRect" | "RedrawWindow" => |p| {
            let h = p.arg(0);
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.needs_paint = true;
            }
            Ok(1)
        },
        "IsWindow" => |p| Ok(p.extra.win.windows.contains_key(&p.arg(0)) as u64),
        "IsWindowVisible" => |p| Ok(p.extra.win.windows.get(&p.arg(0)).map(|w| w.visible as u64).unwrap_or(0)),
        "IsWindowUnicode" => |p| Ok(p.extra.win.windows.get(&p.arg(0)).map(|w| w.unicode as u64).unwrap_or(0)),
        "IsIconic" | "IsZoomed" => |_| Ok(0),
        "GetMessageA" | "GetMessageW" => get_message,
        "PeekMessageA" | "PeekMessageW" => peek_message,
        "TranslateMessage" => |_| Ok(1),
        "DispatchMessageA" | "DispatchMessageW" => |p| {
            let m = p.arg(0);
            let (h, msg, wp, lp) = (p.rd64(m)?, p.rd32(m + 8)? as u64, p.rd64(m + 16)?, p.rd64(m + 24)?);
            send(p, h, msg, wp, lp)
        },
        "PostQuitMessage" => |p| {
            p.extra.win.quit = Some(p.arg(0) as i32);
            Ok(0)
        },
        "PostMessageA" | "PostMessageW" => |p| {
            let (h, m, w, l) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
            post(p, h, m, w, l);
            Ok(1)
        },
        "SetPropA" => |p| set_prop(p, false),
        "SetPropW" => |p| set_prop(p, true),
        "GetPropA" => |p| get_prop(p, false, false),
        "GetPropW" => |p| get_prop(p, true, false),
        "RemovePropA" => |p| get_prop(p, false, true),
        "RemovePropW" => |p| get_prop(p, true, true),
        "OffsetRect" => offset_rect,
        "InflateRect" => inflate_rect,
        "IntersectRect" => intersect_rect,
        "UnionRect" => union_rect,
        "SetRect" => set_rect,
        "SetRectEmpty" => set_rect_empty,
        "CopyRect" => copy_rect,
        "IsRectEmpty" => |p| {
            let r = read_rect(p, p.arg(0))?;
            Ok((r.0 >= r.2 || r.1 >= r.3) as u64)
        },
        "EqualRect" => |p| Ok((read_rect(p, p.arg(0))? == read_rect(p, p.arg(1))?) as u64),
        "PtInRect" => |p| {
            let r = read_rect(p, p.arg(0))?;
            let point = p.arg(1);
            let (x, y) = (point as u32 as i32, (point >> 32) as u32 as i32);
            Ok((x >= r.0 && x < r.2 && y >= r.1 && y < r.3) as u64)
        },
        "SendMessageA" | "SendMessageW" => |p| {
            let (h, m, w, l) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
            send(p, h, m, w, l)
        },
        "DefWindowProcA" | "DefWindowProcW" => |p| {
            let (h, m, w, l) = (p.arg(0), p.arg(1) & 0xffff_ffff, p.arg(2), p.arg(3));
            def_window_proc(p, h, m, w, l)
        },
        "GetClientRect" => |p| {
            let (h, r) = (p.arg(0), p.arg(1));
            let (w, ht) = p.extra.win.windows.get(&h).map(|x| (x.w, x.h)).unwrap_or((0, 0));
            write_rect(p, r, 0, 0, w, ht)?;
            Ok(p.extra.win.windows.contains_key(&h) as u64)
        },
        "GetWindowRect" => |p| {
            let (h, r) = (p.arg(0), p.arg(1));
            let (x, y, w, ht) = p.extra.win.windows.get(&h).map(|x| (x.x, x.y, x.w, x.h)).unwrap_or((0, 0, 0, 0));
            write_rect(p, r, x, y, x + w, y + ht)?;
            Ok(1)
        },
        "AdjustWindowRect" => |p| adjust_window_rect(p, false, false),
        "AdjustWindowRectEx" => |p| adjust_window_rect(p, true, false),
        "AdjustWindowRectExForDpi" => |p| adjust_window_rect(p, true, true),
        "SetWindowPos" => |p| {
            let (h, x, y, cx, cy, flags) = (p.arg(0), p.arg(2) as i32, p.arg(3) as i32, p.arg(4) as i32, p.arg(5) as i32, p.arg(6));
            resize_window(p, h, (flags & 2 == 0).then_some((x, y)), (flags & 1 == 0).then_some((cx, cy)))?;
            Ok(1)
        },
        "MoveWindow" => |p| {
            let (h, x, y, w, ht) = (p.arg(0), p.arg(1) as i32, p.arg(2) as i32, p.arg(3) as i32, p.arg(4) as i32);
            resize_window(p, h, Some((x, y)), Some((w, ht)))?;
            Ok(1)
        },
        "SetWindowTextA" => |p| {
            let (h, s) = (p.arg(0), p.read_cstr(p.arg(1))?);
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.title = s;
            }
            Ok(1)
        },
        "SetWindowTextW" => |p| {
            let (h, s) = (p.arg(0), p.read_wstr(p.arg(1))?);
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.title = s;
            }
            Ok(1)
        },
        "GetWindowTextA" => |p| {
            let (h, buf, n) = (p.arg(0), p.arg(1), p.arg(2) as usize);
            let t = p.extra.win.windows.get(&h).map(|w| w.title.clone()).unwrap_or_default();
            if n > 0 {
                let s = &t[..t.len().min(n - 1)];
                p.write_cstr(buf, s)?;
                return Ok(s.len() as u64);
            }
            Ok(0)
        },
        "SetWindowLongPtrA" | "SetWindowLongPtrW" | "SetWindowLongA" | "SetWindowLongW" => |p| {
            let (h, idx, v) = (p.arg(0), p.arg(1) as i32, p.arg(2));
            let w = p.extra.win.windows.get_mut(&h);
            Ok(match (w, idx) {
                (Some(w), -21) => std::mem::replace(&mut w.user_data, v),
                (Some(w), -4) => std::mem::replace(&mut w.wndproc, v),
                (Some(w), -16) => std::mem::replace(&mut w.style, v as u32) as u64,
                _ => 0,
            })
        },
        "GetWindowLongPtrA" | "GetWindowLongPtrW" | "GetWindowLongA" | "GetWindowLongW" => |p| {
            let (h, idx) = (p.arg(0), p.arg(1) as i32);
            Ok(match (p.extra.win.windows.get(&h), idx) {
                (Some(w), -21) => w.user_data,
                (Some(w), -4) => w.wndproc,
                (Some(w), -16) => w.style as u64,
                _ => 0,
            })
        },
        "GetParent" => |p| Ok(p.extra.win.windows.get(&p.arg(0)).map(|w| w.parent).unwrap_or(0)),
        "GetDlgItem" => |p| {
            let (parent, id) = (p.arg(0), p.arg(1) as u32);
            if !p.extra.win.windows.contains_key(&parent) {
                return Ok(0);
            }
            Ok(p.extra.win.windows.iter().find_map(|(&h, w)| {
                (w.parent == parent && w.style & WS_CHILD != 0 && w.control_id == id).then_some(h)
            }).unwrap_or(0))
        },
        "MapVirtualKeyW" => map_virtual_key_w,
        "ToUnicode" => to_unicode,
        "GetDC" | "GetWindowDC" => |p| Ok(0x7000_0000 + p.arg(0)),
        "ReleaseDC" | "DeleteDC" => |_| Ok(1),
        "BeginPaint" => |p| {
            let (h, ps) = (p.arg(0), p.arg(1));
            p.write_bytes(ps, &[0u8; 72])?;
            p.wr64(ps, 0x7000_0000 + h)?;
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.needs_paint = false;
            }
            Ok(0x7000_0000 + h)
        },
        "EndPaint" => |_| Ok(1),
        "ValidateRect" => |p| {
            if let Some(w) = p.extra.win.windows.get_mut(&p.arg(0)) {
                w.needs_paint = false;
            }
            Ok(1)
        },
        "LoadCursorA" | "LoadCursorW" | "LoadIconA" | "LoadIconW" | "LoadImageA" | "LoadImageW" | "CopyIcon" => |_| Ok(0x6000),
        "SetCursor" | "ShowCursor" | "ClipCursor" => |_| Ok(0),
        "GetCursorPos" => |p| {
            let a = p.arg(0);
            let c = p.extra.win.cursor;
            p.wr32(a, c.0 as u32)?;
            p.wr32(a + 4, c.1 as u32)?;
            Ok(1)
        },
        "SetCursorPos" => |p| {
            p.extra.win.cursor = (p.arg(0) as i32, p.arg(1) as i32);
            Ok(1)
        },
        "ScreenToClient" | "ClientToScreen" => |_| Ok(1),
        "GetAsyncKeyState" | "GetKeyState" => |p| {
            let vk = (p.arg(0) & 0xff) as usize;
            Ok(p.extra.win.keys.get(vk).map(|&d| if d != 0 { 0x8000u64 } else { 0 }).unwrap_or(0))
        },
        "GetKeyboardLayout" => |_| Ok(0x0409_0409), // US English HKL for the current thread.
        "SetWindowsHookExA" | "SetWindowsHookExW" => set_windows_hook,
        "UnhookWindowsHookEx" => |p| Ok(p.extra.win.hooks.remove(&p.arg(0)).is_some() as u64),
        "CallNextHookEx" => |_| Ok(0),
        "GetKeyboardState" => |p| {
            let a = p.arg(0);
            let mut k = p.extra.win.keys.clone();
            k.resize(256, 0);
            p.write_bytes(a, &k)?;
            Ok(1)
        },
        // The runtime has one guest UI thread; this is its captured window handle.
        "GetCapture" => |p| Ok(p.extra.win.capture),
        "SetCapture" => |p| {
            let h = p.arg(0);
            Ok(std::mem::replace(&mut p.extra.win.capture, h))
        },
        "ReleaseCapture" => |p| {
            p.extra.win.capture = 0;
            Ok(1)
        },
        "TrackMouseEvent" => track_mouse_event,
        "SetFocus" => |p| {
            let h = p.arg(0);
            Ok(std::mem::replace(&mut p.extra.win.focus, h))
        },
        "GetFocus" | "GetForegroundWindow" | "GetActiveWindow" => |p| Ok(p.extra.win.main_window),
        "SetForegroundWindow" | "SetActiveWindow" | "BringWindowToTop" => |_| Ok(1),
        "GetSystemMetrics" => |p| Ok(system_metric(p.arg(0) as u32)),
        "GetSystemMetricsForDpi" => |p| {
            let (index, dpi) = (p.arg(0) as u32, p.arg(1) as u32);
            Ok(system_metric(index).saturating_mul(dpi as u64) / 96)
        },
        "MessageBoxA" => |p| {
            let (t, c) = (p.read_cstr(p.arg(1))?, p.read_cstr(p.arg(2))?);
            p.stderr.extend_from_slice(format!("[MessageBox] {c}: {t}\n").as_bytes());
            Ok(1)
        },
        "MessageBoxW" => |p| {
            let (t, c) = (p.read_wstr(p.arg(1))?, p.read_wstr(p.arg(2))?);
            p.stderr.extend_from_slice(format!("[MessageBox] {c}: {t}\n").as_bytes());
            Ok(1)
        },
        "SetTimer" => |p| {
            let (h, id, ms, f) = (p.arg(0), p.arg(1), p.arg(2) as u32, p.arg(3));
            p.extra.win.timers.push((h, id, ms, f));
            Ok(id.max(1))
        },
        "KillTimer" => |p| {
            let (h, id) = (p.arg(0), p.arg(1));
            p.extra.win.timers.retain(|t| !(t.0 == h && t.1 == id));
            Ok(1)
        },
        "MonitorFromWindow" | "MonitorFromPoint" => |_| Ok(0x5000),
        "GetMonitorInfoA" | "GetMonitorInfoW" => |p| {
            let a = p.arg(1);
            p.wr32(a + 4, 0)?;
            p.wr32(a + 8, 0)?;
            p.wr32(a + 12, 1920)?;
            p.wr32(a + 16, 1080)?;
            p.wr32(a + 20, 0)?;
            p.wr32(a + 24, 0)?;
            p.wr32(a + 28, 1920)?;
            p.wr32(a + 32, 1080)?;
            p.wr32(a + 36, 1)?;
            Ok(1)
        },
        "SetProcessDPIAware" | "SetProcessDpiAwarenessContext" | "EnableWindow" | "SetLayeredWindowAttributes" | "SystemParametersInfoA" | "SystemParametersInfoW" => |_| Ok(1),
        "GetDpiForWindow" | "GetDpiForSystem" => |_| Ok(96),
        _ => return None,
    })
}

/// Register or cancel the Win32 TME_LEAVE notification used by GLFW's Win32 backend.
fn track_mouse_event(p: &mut Process) -> R {
    let request = p.arg(0);
    let size = p.rd32(request)?;
    if size < 24 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let flags = p.rd32(request + 4)?;
    let hwnd = p.rd64(request + 8)?;
    if flags != TME_LEAVE && flags != (TME_CANCEL | TME_LEAVE) && flags != TME_CANCEL {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    if !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    }
    if flags & TME_CANCEL != 0 {
        p.extra.win.mouse_tracking.remove(&hwnd);
    } else {
        p.extra.win.mouse_tracking.insert(hwnd);
    }
    Ok(1)
}

fn system_metric(index: u32) -> u64 {
    match index {
        0 | 16 | 76 | 78 => 1920, // screen/fullscreen/virtual width
        1 | 17 | 77 | 79 => 1080, // screen/fullscreen/virtual height
        4 => 23,                  // caption height
        5 | 6 => 1,               // border width / height
        7 | 8 => 3,               // dialog frame width / height
        13 | 14 => 32,            // cursor width / height
        15 => 20,                 // menu height
        28 => 112,                // minimum window width
        29 => 27,                 // minimum window height
        32 | 33 => 8,             // sizing frame width / height
        80 => 1,                  // monitor count
        _ => 0,
    }
}

fn get_window_placement(p: &mut Process) -> R {
    const WINDOWPLACEMENT_SIZE: u32 = 44;
    let (hwnd, placement) = (p.arg(0), p.arg(1));
    if placement == 0 || p.rd32(placement)? != WINDOWPLACEMENT_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let Some((x, y, w, h)) = p.extra.win.windows.get(&hwnd).map(|win| (win.x, win.y, win.w, win.h)) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    p.wr32(placement + 4, 0)?;
    p.wr32(placement + 8, 1)?; // SW_SHOWNORMAL
    p.wr32(placement + 12, 0)?;
    p.wr32(placement + 16, 0)?;
    p.wr32(placement + 20, 0)?;
    p.wr32(placement + 24, 0)?;
    p.wr32(placement + 28, x as u32)?;
    p.wr32(placement + 32, y as u32)?;
    p.wr32(placement + 36, x.wrapping_add(w) as u32)?;
    p.wr32(placement + 40, y.wrapping_add(h) as u32)?;
    Ok(1)
}

fn set_window_placement(p: &mut Process) -> R {
    const WINDOWPLACEMENT_SIZE: u32 = 44;
    let (hwnd, placement) = (p.arg(0), p.arg(1));
    if placement == 0 || p.rd32(placement)? != WINDOWPLACEMENT_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (left, top, right, bottom) = (
        p.rd32(placement + 28)? as i32,
        p.rd32(placement + 32)? as i32,
        p.rd32(placement + 36)? as i32,
        p.rd32(placement + 40)? as i32,
    );
    let visible = p.rd32(placement + 8)? != 0;
    let Some(window) = p.extra.win.windows.get_mut(&hwnd) else {
        p.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(0);
    };
    window.x = left;
    window.y = top;
    window.w = right.saturating_sub(left);
    window.h = bottom.saturating_sub(top);
    window.visible = visible;
    window.needs_paint |= window.visible;
    Ok(1)
}

fn adjust_window_rect(p: &mut Process, extended: bool, dpi_aware: bool) -> R {
    let rect = p.arg(0);
    if rect == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (style, menu, ex_style) = (p.arg(1) as u32, p.arg(2) != 0, if extended { p.arg(3) as u32 } else { 0 });
    let dpi = if dpi_aware { (p.arg(4) as u32).max(1) } else { 96 };
    let scale = |v: u32| ((v as u64 * dpi as u64 + 48) / 96) as i32;
    let thick = style & 0x0004_0000 != 0;
    let border = style & (0x0080_0000 | 0x0040_0000) != 0;
    let edge = ex_style & (0x0000_0200 | 0x0001_0000) != 0;
    let x = scale(if thick { 8 } else if border { 1 } else { 0 }) + scale(edge as u32 * 2);
    let mut y = scale(if thick { 8 } else if border { 1 } else { 0 }) + scale(edge as u32 * 2);
    if style & 0x00c0_0000 != 0 {
        y += scale(system_metric(4) as u32);
    }
    if menu {
        y += scale(system_metric(15) as u32);
    }
    let (left, top, right, bottom) = (
        p.rd32(rect)? as i32,
        p.rd32(rect + 4)? as i32,
        p.rd32(rect + 8)? as i32,
        p.rd32(rect + 12)? as i32,
    );
    p.wr32(rect, left.wrapping_sub(x) as u32)?;
    p.wr32(rect + 4, top.wrapping_sub(y) as u32)?;
    p.wr32(rect + 8, right.wrapping_add(x) as u32)?;
    p.wr32(rect + 12, bottom.wrapping_add(scale(if thick { 8 } else if border { 1 } else { 0 })) as u32)?;
    Ok(1)
}

fn map_virtual_key_w(p: &mut Process) -> R {
    let (code, map_type) = (p.arg(0) as u32, p.arg(1) as u32);
    Ok(map_virtual_key(code, map_type) as u64)
}

fn enum_display_devices_w(p: &mut Process) -> R {
    const DISPLAY_DEVICE_SIZE: u32 = 840;
    let (device, index, out, flags) = (p.arg(0), p.arg(1) as u32, p.arg(2), p.arg(3) as u32);
    if out == 0 || p.rd32(out)? < DISPLAY_DEVICE_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    if index != 0 || flags & !1 != 0 {
        return Ok(0);
    }
    if device != 0 {
        let name = p.read_wstr(device)?;
        if !name.eq_ignore_ascii_case("\\\\.\\DISPLAY1") {
            return Ok(0);
        }
    }

    p.write_bytes(out, &[0; DISPLAY_DEVICE_SIZE as usize])?;
    p.wr32(out, DISPLAY_DEVICE_SIZE)?;
    p.write_wstr(out + 4, "\\\\.\\DISPLAY1")?;
    p.write_wstr(out + 68, "AnyWASM Virtual Display")?;
    p.wr32(out + 324, 0x1 | 0x4)?; // ATTACHED_TO_DESKTOP | PRIMARY_DEVICE
    if flags & 1 != 0 {
        p.write_wstr(out + 328, "\\\\?\\DISPLAY#ANYWASM#0001")?;
    }
    Ok(1)
}

fn enum_display_settings_w(p: &mut Process) -> R {
    const DEVMODEW_SIZE: u16 = 220;
    let (device, mode_num, out) = (p.arg(0), p.arg(1) as u32, p.arg(2));
    if out == 0 || p.rd16(out + 68)? < DEVMODEW_SIZE {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    if device != 0 && !p.read_wstr(device)?.eq_ignore_ascii_case("\\\\.\\DISPLAY1") {
        return Ok(0);
    }

    let (width, height, refresh) = match mode_num {
        0xffff_ffff | 0 => (1920, 1080, 60), // current / registry mode
        1 => (1280, 720, 60),
        2 => (640, 480, 60),
        _ => return Ok(0),
    };
    p.write_bytes(out, &[0; DEVMODEW_SIZE as usize])?;
    p.write_wstr(out, "\\\\.\\DISPLAY1")?;
    p.wr16(out + 64, 0x0401)?; // DM_SPECVERSION
    p.wr16(out + 66, 1)?;
    p.wr16(out + 68, DEVMODEW_SIZE)?;
    p.wr32(out + 72, 0x007c_0000)?; // bits-per-pixel, width, height, flags, frequency
    p.wr32(out + 168, 32)?;
    p.wr32(out + 172, width)?;
    p.wr32(out + 176, height)?;
    p.wr32(out + 180, 0)?;
    p.wr32(out + 184, refresh)?;
    Ok(1)
}

fn enum_display_monitors(p: &mut Process) -> R {
    let (hdc, clip, callback, data) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if callback == 0 {
        p.set_last_error(87); // ERROR_INVALID_PARAMETER
        return Ok(0);
    }
    let (mut left, mut top, mut right, mut bottom) = (0i32, 0i32, 1920i32, 1080i32);
    if clip != 0 {
        let (clip_left, clip_top, clip_right, clip_bottom) = (
            p.rd32(clip)? as i32,
            p.rd32(clip + 4)? as i32,
            p.rd32(clip + 8)? as i32,
            p.rd32(clip + 12)? as i32,
        );
        left = left.max(clip_left);
        top = top.max(clip_top);
        right = right.min(clip_right);
        bottom = bottom.min(clip_bottom);
        if left >= right || top >= bottom {
            return Ok(1);
        }
    }

    let rect = super::kernel32::alloc_block(p, 16, true);
    if rect == 0 {
        return Ok(0);
    }
    p.wr32(rect, left as u32)?;
    p.wr32(rect + 4, top as u32)?;
    p.wr32(rect + 8, right as u32)?;
    p.wr32(rect + 12, bottom as u32)?;
    let result = p.call_guest(callback, &[0x5000, hdc, rect, data]);
    super::kernel32::free_block(p, rect);
    result?;
    Ok(1)
}

fn to_unicode(p: &mut Process) -> R {
    let (vk, scan, key_state, buffer, capacity, flags) = (
        p.arg(0) as u32,
        p.arg(1) as u32,
        p.arg(2),
        p.arg(3),
        p.arg(4) as u32 as i32,
        p.arg(5) as u32,
    );
    let _ = (scan, flags);
    if capacity <= 0 || buffer == 0 {
        return Ok(0);
    }
    let state: [u8; 256] = if key_state == 0 { [0; 256] } else { p.read_bytes(key_state, 256)?.try_into().unwrap() };
    let shift = [0x10, 0xa0, 0xa1].iter().any(|&key| state[key] & 0x80 != 0);
    let control = [0x11, 0xa2, 0xa3].iter().any(|&key| state[key] & 0x80 != 0);
    let alt = [0x12, 0xa4, 0xa5].iter().any(|&key| state[key] & 0x80 != 0);
    if control || alt {
        return Ok(0);
    }
    let caps = state[0x14] & 1 != 0;
    let Some(ch) = translate_us_key(vk, shift, caps) else { return Ok(0) };
    p.wr16(buffer, ch)?;
    Ok(1)
}

fn translate_us_key(vk: u32, shift: bool, caps: bool) -> Option<u16> {
    if (0x41..=0x5a).contains(&vk) {
        let upper = shift ^ caps;
        return Some(if upper { vk as u16 } else { (vk + 0x20) as u16 });
    }
    if (0x30..=0x39).contains(&vk) {
        let shifted = b")!@#$%^&*("[(vk - 0x30) as usize] as u16;
        return Some(if shift { shifted } else { vk as u16 });
    }
    if (0x60..=0x69).contains(&vk) {
        return Some((b'0' + (vk - 0x60) as u8) as u16);
    }
    Some(match vk {
        0x08 => 0x0008,
        0x09 => 0x0009,
        0x0d => 0x000d,
        0x20 => 0x0020,
        0xba => if shift { b':' as u16 } else { b';' as u16 },
        0xbb => if shift { b'+' as u16 } else { b'=' as u16 },
        0xbc => if shift { b'<' as u16 } else { b',' as u16 },
        0xbd => if shift { b'_' as u16 } else { b'-' as u16 },
        0xbe => if shift { b'>' as u16 } else { b'.' as u16 },
        0xbf => if shift { b'?' as u16 } else { b'/' as u16 },
        0xc0 => if shift { b'~' as u16 } else { b'`' as u16 },
        0xdb => if shift { b'{' as u16 } else { b'[' as u16 },
        0xdc => if shift { b'|' as u16 } else { b'\\' as u16 },
        0xdd => if shift { b'}' as u16 } else { b']' as u16 },
        0xde => if shift { b'"' as u16 } else { b'\'' as u16 },
        0xe2 => if shift { b'|' as u16 } else { b'\\' as u16 },
        _ => return None,
    })
}

fn map_virtual_key(code: u32, map_type: u32) -> u32 {
    match map_type {
        0 => vk_to_scan(code).map(|(scan, _)| scan as u32).unwrap_or(0),
        1 => scan_to_vk(code, false),
        2 => vk_to_char(code),
        3 => scan_to_vk(code, true),
        4 => vk_to_scan(code).map(|(scan, extended)| if extended { 0xe000 | scan as u32 } else { scan as u32 }).unwrap_or(0),
        _ => 0,
    }
}

fn vk_to_scan(vk: u32) -> Option<(u8, bool)> {
    const LETTERS: [u8; 26] = [0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c];
    const KEYPAD: [u8; 10] = [0x52, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49];
    if (0x41..=0x5a).contains(&vk) {
        return Some((LETTERS[(vk - 0x41) as usize], false));
    }
    if (0x60..=0x69).contains(&vk) {
        return Some((KEYPAD[(vk - 0x60) as usize], false));
    }
    if (0x70..=0x79).contains(&vk) {
        return Some((0x3b + (vk - 0x70) as u8, false));
    }
    if (0x30..=0x39).contains(&vk) {
        return Some((match vk { 0x30 => 0x0b, _ => 0x02 + (vk - 0x31) as u8 }, false));
    }
    Some(match vk {
        0x08 => (0x0e, false), // backspace
        0x09 => (0x0f, false), // tab
        0x0d => (0x1c, false), // enter
        0x10 | 0xa0 => (0x2a, false),
        0xa1 => (0x36, false),
        0x11 | 0xa2 => (0x1d, false),
        0xa3 => (0x1d, true),
        0x12 | 0xa4 => (0x38, false),
        0xa5 => (0x38, true),
        0x13 => (0x1d, true), // pause (E1)
        0x14 => (0x3a, false), // caps lock
        0x1b => (0x01, false), // escape
        0x20 => (0x39, false), // space
        0x21 => (0x49, false), // page up
        0x22 => (0x51, false), // page down
        0x23 => (0x4f, false), // end
        0x24 => (0x47, false), // home
        0x25 => (0x4b, false), // left
        0x26 => (0x48, false), // up
        0x27 => (0x4d, false), // right
        0x28 => (0x50, false), // down
        0x2c => (0x54, false), // print screen
        0x2d => (0x52, false), // insert
        0x2e => (0x53, false), // delete
        0x5b => (0x5b, true),  // left Windows
        0x5c => (0x5c, true),  // right Windows
        0x5d => (0x5d, true),  // applications
        0x6a => (0x37, false), // keypad multiply
        0x6b => (0x4e, false), // keypad add
        0x6d => (0x4a, false), // keypad subtract
        0x6e => (0x53, false), // keypad decimal
        0x6f => (0x35, true),  // keypad divide
        0x7a => (0x57, false), // F11
        0x7b => (0x58, false), // F12
        0x90 => (0x45, false), // num lock
        0x91 => (0x46, false), // scroll lock
        0xba => (0x27, false), // OEM 1 ;
        0xbb => (0x0d, false), // OEM plus
        0xbc => (0x33, false), // OEM comma
        0xbd => (0x0c, false), // OEM minus
        0xbe => (0x34, false), // OEM period
        0xbf => (0x35, false), // OEM 2 /
        0xc0 => (0x29, false), // OEM 3 `
        0xdb => (0x1a, false), // OEM 4 [
        0xdc => (0x2b, false), // OEM 5 backslash
        0xdd => (0x1b, false), // OEM 6 ]
        0xde => (0x28, false), // OEM 7 '
        0xe2 => (0x56, false), // OEM 102
        _ => return None,
    })
}

fn scan_to_vk(code: u32, distinguish: bool) -> u32 {
    let prefix = (code >> 8) as u8;
    let scan = code as u8;
    if prefix != 0 && prefix != 0xe0 && prefix != 0xe1 {
        return 0;
    }
    if prefix == 0xe1 && scan == 0x1d {
        return 0x13; // pause
    }
    if prefix == 0xe0 {
        return match scan {
            0x1d => if distinguish { 0xa3 } else { 0x11 },
            0x38 => if distinguish { 0xa5 } else { 0x12 },
            0x47 => 0x24,
            0x48 => 0x26,
            0x49 => 0x21,
            0x4b => 0x25,
            0x4d => 0x27,
            0x4f => 0x23,
            0x50 => 0x28,
            0x51 => 0x22,
            0x52 => 0x2d,
            0x53 => 0x2e,
            0x5b => 0x5b,
            0x5c => 0x5c,
            0x5d => 0x5d,
            _ => 0,
        };
    }
    match scan {
        0x2a => return if distinguish { 0xa0 } else { 0x10 },
        0x36 => return if distinguish { 0xa1 } else { 0x10 },
        0x1d => return if distinguish { 0xa2 } else { 0x11 },
        0x38 => return if distinguish { 0xa4 } else { 0x12 },
        _ => {}
    }
    (0..=0xff).find_map(|vk| vk_to_scan(vk).filter(|(s, extended)| *s == scan && !extended).map(|_| vk)).unwrap_or(0)
}

fn vk_to_char(vk: u32) -> u32 {
    match vk {
        0x30..=0x39 | 0x41..=0x5a => vk,
        0x20 => 0x20,
        0xba => b';' as u32,
        0xbb => b'=' as u32,
        0xbc => b',' as u32,
        0xbd => b'-' as u32,
        0xbe => b'.' as u32,
        0xbf => b'/' as u32,
        0xc0 => b'`' as u32,
        0xdb => b'[' as u32,
        0xdc => b'\\' as u32,
        0xdd => b']' as u32,
        0xde => b'\'' as u32,
        0xe2 => b'\\' as u32,
        _ => 0,
    }
}

fn write_rect(p: &mut Process, a: u64, l: i32, t: i32, r: i32, b: i32) -> Result<(), Stop> {
    p.wr32(a, l as u32)?;
    p.wr32(a + 4, t as u32)?;
    p.wr32(a + 8, r as u32)?;
    p.wr32(a + 12, b as u32)
}

fn read_rect(p: &Process, a: u64) -> Result<(i32, i32, i32, i32), Stop> {
    Ok((p.rd32(a)? as i32, p.rd32(a + 4)? as i32, p.rd32(a + 8)? as i32, p.rd32(a + 12)? as i32))
}

fn offset_rect(p: &mut Process) -> R {
    let (a, dx, dy) = (p.arg(0), p.arg(1) as i32, p.arg(2) as i32);
    let (l, t, r, b) = read_rect(p, a)?;
    write_rect(p, a, l.wrapping_add(dx), t.wrapping_add(dy), r.wrapping_add(dx), b.wrapping_add(dy))?;
    Ok(1)
}

fn inflate_rect(p: &mut Process) -> R {
    let (a, dx, dy) = (p.arg(0), p.arg(1) as i32, p.arg(2) as i32);
    let (l, t, r, b) = read_rect(p, a)?;
    write_rect(p, a, l.wrapping_sub(dx), t.wrapping_sub(dy), r.wrapping_add(dx), b.wrapping_add(dy))?;
    Ok(1)
}

fn intersect_rect(p: &mut Process) -> R {
    let (dst, a, b) = (p.arg(0), p.arg(1), p.arg(2));
    let (r1, r2) = (read_rect(p, a)?, read_rect(p, b)?);
    let out = (r1.0.max(r2.0), r1.1.max(r2.1), r1.2.min(r2.2), r1.3.min(r2.3));
    let nonempty = out.0 < out.2 && out.1 < out.3;
    if nonempty {
        write_rect(p, dst, out.0, out.1, out.2, out.3)?;
    } else {
        write_rect(p, dst, 0, 0, 0, 0)?;
    }
    Ok(nonempty as u64)
}

fn union_rect(p: &mut Process) -> R {
    let (dst, a, b) = (p.arg(0), p.arg(1), p.arg(2));
    let (r1, r2) = (read_rect(p, a)?, read_rect(p, b)?);
    let empty1 = r1.0 >= r1.2 || r1.1 >= r1.3;
    let empty2 = r2.0 >= r2.2 || r2.1 >= r2.3;
    let out = match (empty1, empty2) {
        (true, true) => (0, 0, 0, 0),
        (true, false) => r2,
        (false, true) => r1,
        (false, false) => (r1.0.min(r2.0), r1.1.min(r2.1), r1.2.max(r2.2), r1.3.max(r2.3)),
    };
    write_rect(p, dst, out.0, out.1, out.2, out.3)?;
    Ok((!empty1 || !empty2) as u64)
}

fn set_rect(p: &mut Process) -> R {
    let (a, l, t, r, b) = (p.arg(0), p.arg(1) as i32, p.arg(2) as i32, p.arg(3) as i32, p.arg(4) as i32);
    write_rect(p, a, l, t, r, b)?;
    Ok(1)
}

fn set_rect_empty(p: &mut Process) -> R {
    let a = p.arg(0);
    write_rect(p, a, 0, 0, 0, 0)?;
    Ok(1)
}

fn copy_rect(p: &mut Process) -> R {
    let (dst, src) = (p.arg(0), p.arg(1));
    let r = read_rect(p, src)?;
    write_rect(p, dst, r.0, r.1, r.2, r.3)?;
    Ok(1)
}

fn register_class(p: &mut Process, wide: bool, ex: bool) -> R {
    let a = p.arg(0);
    // WNDCLASS and WNDCLASSEX have different field offsets in PE32. In PE32+,
    // the pointer-sized fields and alignment move the same fields again.
    let x86 = p.cpu.mode == CpuMode::X86_32;
    let wndproc_off = if ex || !x86 { 8 } else { 4 };
    let name_off = match (x86, ex) {
        (true, false) => 36,
        (true, true) => 40,
        (false, _) => 64,
    };
    let wndproc = p.rdptr(a + wndproc_off)?;
    let name_ptr = p.rdptr(a + name_off)?;
    let name = lc(&class_name(p, name_ptr, wide)?);
    p.extra.win.next_atom += 1;
    let atom = 0xc000 + p.extra.win.next_atom;
    p.extra.win.atoms.insert(atom, name.clone());
    p.extra.win.classes.insert(name.clone(), Class { wndproc, name, unicode: wide });
    Ok(atom as u64)
}

fn wsprintf_a(p: &mut Process) -> R {
    let (buffer, format_ptr) = (p.arg(0), p.arg(1));
    let format = p.read_cstr(format_ptr)?;
    let output = printf::format(
        p,
        format.as_bytes(),
        &mut printf::SlotArgs(2),
        false,
    )?;
    p.write_bytes(buffer, &output)?;
    p.wr8(buffer + output.len() as u64, 0)?;
    Ok(output.len() as u64)
}

fn char_case_a(p: &mut Process, upper: bool, buffered: bool) -> R {
    let pointer = p.arg(0);
    if pointer == 0 {
        return Ok(0);
    }
    if !buffered && pointer <= 0xffff {
        let byte = pointer as u8;
        return Ok(if upper {
            byte.to_ascii_uppercase() as u64
        } else {
            byte.to_ascii_lowercase() as u64
        });
    }
    let bytes = if buffered {
        p.read_bytes(pointer, usize::try_from(p.arg(1)).unwrap_or(usize::MAX))?
    } else {
        let mut bytes = Vec::new();
        for offset in 0..(1 << 20) {
            let byte = p.rd8(pointer + offset)?;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        bytes
    };
    let bytes: Vec<u8> = bytes
        .into_iter()
        .map(|byte| if upper { byte.to_ascii_uppercase() } else { byte.to_ascii_lowercase() })
        .collect();
    if buffered {
        let len = bytes.len();
        p.write_bytes(pointer, &bytes)?;
        Ok(len as u64)
    } else {
        p.write_bytes(pointer, &bytes)?;
        p.wr8(pointer + bytes.len() as u64, 0)?;
        Ok(pointer)
    }
}

fn create_window(p: &mut Process, wide: bool) -> R {
    let (ex_style, cls, title, style) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3) as u32);
    let (x, y, w, h) = (p.arg(4) as i32, p.arg(5) as i32, p.arg(6) as i32, p.arg(7) as i32);
    let (parent, menu, inst, param) = (p.arg(8), p.arg(9), p.arg(10), p.arg(11));
    let cname = if cls < 0x10000 && cls >= 0xc000 { p.extra.win.atoms.get(&(cls as u16)).cloned().unwrap_or_default() } else { lc(&class_name(p, cls, wide)?) };
    let Some(class) = p.extra.win.classes.get(&cname).cloned().or_else(|| builtin_class(&cname)) else {
        p.set_last_error(1407); // ERROR_CANNOT_FIND_WND_CLASS
        return Ok(0);
    };
    let title_s = if title == 0 { String::new() } else if wide { p.read_wstr(title)? } else { p.read_cstr(title)? };
    let cw = |v: i32, d: i32| if v == i32::MIN || v == 0x8000_0000u32 as i32 { d } else { v };
    let (w, h) = (cw(w, 1280), cw(h, 720));
    let (x, y) = (cw(x, 0), cw(y, 0));
    p.extra.win.next = p.extra.win.next.max(HWND_BASE) + 0x10;
    let hwnd = p.extra.win.next;
    let gpu_id = p.extra.gpu.new_id();
    let control_id = if parent != 0 && style & WS_CHILD != 0 { menu as u32 } else { 0 };
    p.extra.win.windows.insert(hwnd, Window { class: cname, wndproc: class.wndproc, title: title_s.clone(), x, y, w, h, visible: false, style, user_data: 0, parent, control_id, unicode: class.unicode, needs_paint: false, gpu_id });
    if p.extra.win.main_window == 0 && parent == 0 {
        p.extra.win.main_window = hwnd;
        p.extra.win.focus = hwnd;
    }
    p.extra.gpu.emit(op::WINDOW, Pk::new().u32(gpu_id).u32(w as u32).u32(h as u32).bytes(title_s.as_bytes()));
    // CREATESTRUCT (x64, 80 bytes): lpCreateParams, hInstance, hMenu, hwndParent, cy, cx, y, x, style, lpszName, lpszClass, dwExStyle
    let cs = kernel32::alloc_block(p, 96, true);
    p.wr64(cs, param)?;
    p.wr64(cs + 8, inst)?;
    p.wr64(cs + 24, parent)?;
    p.wr32(cs + 32, h as u32)?;
    p.wr32(cs + 36, w as u32)?;
    p.wr32(cs + 40, y as u32)?;
    p.wr32(cs + 44, x as u32)?;
    p.wr32(cs + 48, style)?;
    p.wr64(cs + 56, title)?;
    p.wr64(cs + 64, cls)?;
    p.wr32(cs + 72, ex_style as u32)?;
    let ok = send(p, hwnd, WM_NCCREATE, 0, cs)?;
    if ok == 0 {
        p.extra.win.windows.remove(&hwnd);
        return Ok(0);
    }
    let r = send(p, hwnd, WM_CREATE, 0, cs)?;
    kernel32::free_block(p, cs);
    if r as i32 == -1 {
        p.extra.win.windows.remove(&hwnd);
        return Ok(0);
    }
    send(p, hwnd, WM_SIZE, 0, ((h as u64) << 16) | (w as u64 & 0xffff))?;
    Ok(hwnd)
}

/// System classes created by name (BUTTON, EDIT, STATIC...) behave as inert windows for now.
fn builtin_class(name: &str) -> Option<Class> {
    matches!(name, "button" | "edit" | "static" | "combobox" | "listbox" | "scrollbar" | "#32770").then(|| Class { wndproc: 0, name: name.to_string(), unicode: true })
}

fn destroy_window(p: &mut Process, h: u64) -> Result<(), Stop> {
    if !p.extra.win.windows.contains_key(&h) {
        return Ok(());
    }
    p.extra.win.mouse_tracking.remove(&h);
    send(p, h, WM_DESTROY, 0, 0)?;
    send(p, h, WM_NCDESTROY, 0, 0)?;
    p.extra.win.windows.remove(&h);
    p.extra.win.props.retain(|(hwnd, _), _| *hwnd != h);
    if p.extra.win.main_window == h {
        p.extra.win.main_window = 0;
    }
    Ok(())
}

fn prop_name(p: &Process, ptr: u64, wide: bool) -> Result<String, Stop> {
    if ptr < 0x10000 {
        Ok(format!("#{}", ptr as u16))
    } else if wide {
        p.read_wstr(ptr)
    } else {
        p.read_cstr(ptr)
    }
}

fn set_prop(p: &mut Process, wide: bool) -> R {
    let (hwnd, name_ptr, value) = (p.arg(0), p.arg(1), p.arg(2));
    if name_ptr == 0 || !p.extra.win.windows.contains_key(&hwnd) {
        p.set_last_error(if name_ptr == 0 { 87 } else { 1400 });
        return Ok(0);
    }
    let name = prop_name(p, name_ptr, wide)?;
    p.extra.win.props.insert((hwnd, name), value);
    Ok(1)
}

fn get_prop(p: &mut Process, wide: bool, remove: bool) -> R {
    let (hwnd, name_ptr) = (p.arg(0), p.arg(1));
    if name_ptr == 0 {
        return Ok(0);
    }
    let name = prop_name(p, name_ptr, wide)?;
    Ok(if remove {
        p.extra.win.props.remove(&(hwnd, name)).unwrap_or(0)
    } else {
        p.extra.win.props.get(&(hwnd, name)).copied().unwrap_or(0)
    })
}

fn resize_window(p: &mut Process, h: u64, pos: Option<(i32, i32)>, size: Option<(i32, i32)>) -> Result<(), Stop> {
    let mut changed = None;
    if let Some(w) = p.extra.win.windows.get_mut(&h) {
        if let Some((x, y)) = pos {
            w.x = x;
            w.y = y;
        }
        if let Some((cx, cy)) = size {
            if (cx, cy) != (w.w, w.h) {
                w.w = cx;
                w.h = cy;
                changed = Some((cx, cy, w.gpu_id));
            }
        }
    }
    if let Some((cx, cy, id)) = changed {
        p.extra.gpu.emit(op::WINDOW, Pk::new().u32(id).u32(cx as u32).u32(cy as u32));
        send(p, h, WM_SIZE, 0, ((cy as u64) << 16) | (cx as u64 & 0xffff))?;
    }
    Ok(())
}

pub fn send(p: &mut Process, hwnd: u64, msg: u64, wparam: u64, lparam: u64) -> R {
    let Some(w) = p.extra.win.windows.get(&hwnd) else { return Ok(0) };
    let wndproc = w.wndproc;
    if wndproc == 0 {
        return def_window_proc(p, hwnd, msg, wparam, lparam);
    }
    p.call_guest(wndproc, &[hwnd, msg, wparam, lparam])
}

fn post(p: &mut Process, hwnd: u64, msg: u64, wparam: u64, lparam: u64) {
    let t = ((p.cpu.icount + p.extra.sleep_adjust) / 100_000) as u32;
    let pt = p.extra.win.cursor;
    p.extra.win.queue.push_back(Msg { hwnd, msg, wparam, lparam, time: t, pt });
}

fn paint_now(p: &mut Process, h: u64) -> Result<(), Stop> {
    if p.extra.win.windows.get(&h).map(|w| w.needs_paint && w.visible).unwrap_or(false) {
        send(p, h, WM_PAINT, 0, 0)?;
    }
    Ok(())
}

fn def_window_proc(p: &mut Process, h: u64, msg: u64, _w: u64, _l: u64) -> R {
    match msg {
        WM_NCCREATE => Ok(1),
        WM_CLOSE => {
            destroy_window(p, h)?;
            Ok(0)
        }
        WM_ERASEBKGND => Ok(1),
        WM_PAINT => {
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.needs_paint = false;
            }
            Ok(0)
        }
        _ => Ok(0),
    }
}

/// Fill the `MSG` at `out`. Returns false when nothing is available.
fn fetch(p: &mut Process, out: u64, filter_hwnd: u64, remove: bool) -> Result<bool, Stop> {
    // Queued messages first (input, posted messages).
    let idx = p.extra.win.queue.iter().position(|m| filter_hwnd == 0 || m.hwnd == filter_hwnd);
    let msg = if let Some(i) = idx {
        let m = p.extra.win.queue[i];
        if remove {
            p.extra.win.queue.remove(i);
        }
        Some(m)
    } else if let Some(code) = p.extra.win.quit {
        if remove {
            p.extra.win.quit = None;
            p.extra.win.queue.clear();
        }
        Some(Msg { hwnd: 0, msg: WM_QUIT, wparam: code as u64, lparam: 0, time: 0, pt: (0, 0) })
    } else if let Some((&h, _)) = p.extra.win.windows.iter().find(|(_, w)| w.needs_paint && w.visible) {
        // WM_PAINT is generated lazily, like Windows does when the queue is otherwise empty.
        if remove {
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.needs_paint = false;
            }
        }
        Some(Msg { hwnd: h, msg: WM_PAINT, wparam: 0, lparam: 0, time: 0, pt: (0, 0) })
    } else {
        None
    };
    let Some(m) = msg else { return Ok(false) };
    p.wr64(out, m.hwnd)?;
    p.wr32(out + 8, m.msg as u32)?;
    p.wr32(out + 12, 0)?;
    p.wr64(out + 16, m.wparam)?;
    p.wr64(out + 24, m.lparam)?;
    p.wr32(out + 32, m.time)?;
    p.wr32(out + 36, m.pt.0 as u32)?;
    p.wr32(out + 40, m.pt.1 as u32)?;
    Ok(true)
}

fn get_message(p: &mut Process) -> R {
    let (m, h) = (p.arg(0), p.arg(1));
    if fetch(p, m, h, true)? {
        let msg = p.rd32(m + 8)? as u64;
        return Ok((msg != WM_QUIT) as u64);
    }
    // Nothing to do: hand control back to the host and retry this call when input arrives.
    Err(Stop::Idle)
}

fn peek_message(p: &mut Process) -> R {
    let (m, h, flags) = (p.arg(0), p.arg(1), p.arg(4));
    Ok(fetch(p, m, h, flags & PM_REMOVE != 0)? as u64)
}

/// Select the browser input target, skipping hidden GLFW helper windows.
fn input_target_window(win: &WinState) -> u64 {
    let is_visible_top_level = |h: u64| {
        win.windows
            .get(&h)
            .map(|w| w.visible && w.parent == 0)
            .unwrap_or(false)
    };
    if is_visible_top_level(win.focus) {
        win.focus
    } else if is_visible_top_level(win.main_window) {
        win.main_window
    } else {
        win.windows
            .iter()
            .find_map(|(&h, w)| (w.visible && w.parent == 0).then_some(h))
            .unwrap_or(win.main_window)
    }
}

/// Translate a browser input event into window messages. Kinds:
/// 0 key down (a=vk), 1 key up, 2 char (a=utf16), 3 mouse move (a=x,b=y), 4 button down (a=btn 0/1/2,b=x,c=y),
/// 5 button up, 6 wheel (a=delta, b=x, c=y), 7 resize (a=w,b=h), 8 focus (a=1/0), 9 close request,
/// 10 mouse leave.
fn invoke_low_level_hooks(
    p: &mut Process,
    hook_type: i32,
    wparam: u64,
    point: (i32, i32),
    value: u32,
    flags: u32,
) -> Result<bool, Stop> {
    let hooks: Vec<Hook> = p
        .extra
        .win
        .hooks
        .values()
        .copied()
        .filter(|hook| {
            hook.hook_type == hook_type
                && (hook.thread_id == 0 || hook.thread_id == p.current_thread_id)
        })
        .collect();
    if hooks.is_empty() {
        return Ok(false);
    }

    let is_x86 = p.cpu.mode == CpuMode::X86_32;
    let pointer_bytes = if is_x86 { 4 } else { 8 };
    let now = ((p.cpu.icount + p.extra.sleep_adjust) / 100_000) as u32;
    let (size, data) = if hook_type == 14 {
        // MSLLHOOKSTRUCT: POINT, mouseData, flags, time, ULONG_PTR dwExtraInfo.
        let extra_offset = if is_x86 { 20 } else { 24 };
        let mut bytes = vec![0; extra_offset + pointer_bytes];
        bytes[0..4].copy_from_slice(&(point.0 as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&(point.1 as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&value.to_le_bytes());
        bytes[12..16].copy_from_slice(&flags.to_le_bytes());
        bytes[16..20].copy_from_slice(&now.to_le_bytes());
        (bytes.len(), bytes)
    } else {
        // KBDLLHOOKSTRUCT: vkCode, scanCode, flags, time, ULONG_PTR dwExtraInfo.
        let extra_offset = 16;
        let mut bytes = vec![0; extra_offset + pointer_bytes];
        bytes[0..4].copy_from_slice(&value.to_le_bytes());
        bytes[4..8].copy_from_slice(&map_virtual_key(value, 0).to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[12..16].copy_from_slice(&now.to_le_bytes());
        (bytes.len(), bytes)
    };
    let event = p.alloc_virtual(size as u64, PROT_RW);
    p.write_bytes(event, &data)?;
    let mut suppress = false;
    for hook in hooks {
        if p.call_guest(hook.callback, &[0, wparam, event])? != 0 {
            suppress = true;
            break;
        }
    }
    p.mem.unmap(event, 0x1000);
    p.vmem.remove(&event);
    Ok(suppress)
}

pub fn input_event(p: &mut Process, kind: u32, a: i64, b: i64, c: i64) -> Result<(), Stop> {
    let h = input_target_window(&p.extra.win);
    let packed = |x: i64, y: i64| ((y as u64 & 0xffff) << 16) | (x as u64 & 0xffff);
    match kind {
        0 | 1 => {
            if p.extra.win.keys.len() < 256 {
                p.extra.win.keys.resize(256, 0);
            }
            p.extra.win.keys[(a & 0xff) as usize] = (kind == 0) as u8;
            let up = kind == 1;
            let msg = if up { WM_KEYUP } else { WM_KEYDOWN };
            let key_flags = if up { 0x80 } else { 0 };
            if invoke_low_level_hooks(p, 13, msg, (0, 0), a as u32, key_flags)? || h == 0 {
                return Ok(());
            }
            post(p, h, msg, a as u64, if up { 0xc000_0001 } else { 1 });
        }
        2 => {
            if h != 0 {
                post(p, h, WM_CHAR, a as u64, 1);
            }
        }
        3 => {
            if p.extra.win.mouse_position_valid {
                p.extra.win.mouse_dx = p.extra.win.mouse_dx.saturating_add(a as i32 - p.extra.win.cursor.0);
                p.extra.win.mouse_dy = p.extra.win.mouse_dy.saturating_add(b as i32 - p.extra.win.cursor.1);
            }
            p.extra.win.cursor = (a as i32, b as i32);
            p.extra.win.mouse_position_valid = true;
            if !invoke_low_level_hooks(p, 14, WM_MOUSEMOVE, (a as i32, b as i32), 0, 0)? && h != 0 {
                post(p, h, WM_MOUSEMOVE, 0, packed(a, b));
            }
        }
        4 | 5 => {
            let (m_down, m_up) = match a {
                0 => (WM_LBUTTONDOWN, WM_LBUTTONUP),
                1 => (WM_MBUTTONDOWN, WM_MBUTTONUP),
                _ => (WM_RBUTTONDOWN, WM_RBUTTONUP),
            };
            let vk = [1usize, 4, 2][a.clamp(0, 2) as usize];
            if p.extra.win.keys.len() < 256 {
                p.extra.win.keys.resize(256, 0);
            }
            p.extra.win.keys[vk] = (kind == 4) as u8;
            let msg = if kind == 4 { m_down } else { m_up };
            if !invoke_low_level_hooks(p, 14, msg, (b as i32, c as i32), 0, 0)? && h != 0 {
                post(p, h, msg, 0, packed(b, c));
            }
        }
        6 => {
            p.extra.win.mouse_wheel = p.extra.win.mouse_wheel.saturating_add(a as i32);
            let mouse_data = (a as i32 as u32) << 16;
            if !invoke_low_level_hooks(p, 14, WM_MOUSEWHEEL, (b as i32, c as i32), mouse_data, 0)? && h != 0 {
                post(p, h, WM_MOUSEWHEEL, ((a as u64) & 0xffff) << 16, packed(b, c));
            }
        }
        7 => {
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.w = a as i32;
                w.h = b as i32;
            }
            if h != 0 {
                post(p, h, WM_SIZE, 0, packed(a, b));
            }
        }
        8 if h != 0 => post(p, h, if a != 0 { WM_SETFOCUS } else { WM_KILLFOCUS }, 0, 0),
        9 if h != 0 => post(p, h, WM_CLOSE, 0, 0),
        10 if h != 0 && p.extra.win.mouse_tracking.remove(&h) => post(p, h, WM_MOUSELEAVE, 0, 0),
        10 => {}
        11 => {
            p.extra.win.mouse_dx = p.extra.win.mouse_dx.saturating_add(a as i32);
            p.extra.win.mouse_dy = p.extra.win.mouse_dy.saturating_add(b as i32);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_window(visible: bool, parent: u64) -> Window {
        Window {
            class: "test".into(),
            wndproc: 0,
            title: String::new(),
            x: 0,
            y: 0,
            w: 640,
            h: 480,
            visible,
            style: 0,
            user_data: 0,
            parent,
            control_id: 0,
            unicode: true,
            needs_paint: false,
            gpu_id: 0,
        }
    }

    #[test]
    fn browser_input_targets_visible_game_window_after_hidden_glfw_helper() {
        let helper = HWND_BASE;
        let game = HWND_BASE + 0x10;
        let mut win = WinState::default();
        win.windows.insert(helper, test_window(false, 0));
        win.windows.insert(game, test_window(true, 0));
        win.main_window = helper;
        win.focus = helper;

        assert_eq!(input_target_window(&win), game);
    }
}
