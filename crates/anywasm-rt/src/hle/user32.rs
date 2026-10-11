//! user32 subset: window classes, windows, message queue/pump, input state, plus winmm timers.
//!
//! Windows are virtual; the first top-level window is mirrored to the browser canvas through the
//! GPU command stream (`gpu::op::WINDOW`). Window procedures are real guest code, entered with
//! `Process::call_guest`. `GetMessage` with an empty queue returns `Stop::Idle` *without* advancing
//! the guest, so the host can push input and resume at the same instruction.

use super::gpu::{op, Pk};
use super::*;
use crate::process::*;
use std::collections::{HashMap, VecDeque};

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

const PM_REMOVE: u64 = 1;
const HWND_BASE: u64 = 0x0001_0010;

#[derive(Clone)]
pub struct Class {
    pub wndproc: u64,
    pub name: String,
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
    pub focus: u64,
    pub capture: u64,
    pub main_window: u64,
    pub timers: Vec<(u64, u64, u32, u64)>,
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
        "UnregisterClassA" | "UnregisterClassW" => |_| Ok(1),
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
        "UpdateWindow" => |p| {
            let h = p.arg(0);
            paint_now(p, h)?;
            Ok(1)
        },
        "InvalidateRect" | "RedrawWindow" => |p| {
            let h = p.arg(0);
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.needs_paint = true;
            }
            Ok(1)
        },
        "IsWindow" => |p| Ok(p.extra.win.windows.contains_key(&p.arg(0)) as u64),
        "IsWindowVisible" => |p| Ok(p.extra.win.windows.get(&p.arg(0)).map(|w| w.visible as u64).unwrap_or(0)),
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
        "AdjustWindowRect" | "AdjustWindowRectEx" => |_| Ok(1),
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
        "SetFocus" => |p| {
            let h = p.arg(0);
            Ok(std::mem::replace(&mut p.extra.win.focus, h))
        },
        "GetFocus" | "GetForegroundWindow" | "GetActiveWindow" => |p| Ok(p.extra.win.main_window),
        "SetForegroundWindow" | "SetActiveWindow" | "BringWindowToTop" => |_| Ok(1),
        "GetSystemMetrics" => |p| {
            Ok(match p.arg(0) {
                0 => 1920,
                1 => 1080,
                4 => 23,
                5 | 6 => 1,
                7 | 8 => 3,
                15 => 20,
                16 => 1920,
                17 => 1057,
                78 => 1920,
                79 => 1080,
                _ => 0,
            })
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

fn write_rect(p: &mut Process, a: u64, l: i32, t: i32, r: i32, b: i32) -> Result<(), Stop> {
    p.wr32(a, l as u32)?;
    p.wr32(a + 4, t as u32)?;
    p.wr32(a + 8, r as u32)?;
    p.wr32(a + 12, b as u32)
}

fn register_class(p: &mut Process, wide: bool, ex: bool) -> R {
    let a = p.arg(0);
    // WNDCLASSEX has cbSize first, shifting every field by 8 bytes relative to WNDCLASS.
    let off = if ex { 8 } else { 0 };
    let wndproc = p.rd64(a + off)?;
    let name_ptr = p.rd64(a + 56 + off)?;
    let name = lc(&class_name(p, name_ptr, wide)?);
    p.extra.win.next_atom += 1;
    let atom = 0xc000 + p.extra.win.next_atom;
    p.extra.win.atoms.insert(atom, name.clone());
    p.extra.win.classes.insert(name.clone(), Class { wndproc, name });
    Ok(atom as u64)
}

fn create_window(p: &mut Process, wide: bool) -> R {
    let (ex_style, cls, title, style) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3) as u32);
    let (x, y, w, h) = (p.arg(4) as i32, p.arg(5) as i32, p.arg(6) as i32, p.arg(7) as i32);
    let (parent, _menu, inst, param) = (p.arg(8), p.arg(9), p.arg(10), p.arg(11));
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
    p.extra.win.windows.insert(hwnd, Window { class: cname, wndproc: class.wndproc, title: title_s.clone(), x, y, w, h, visible: false, style, user_data: 0, parent, needs_paint: false, gpu_id });
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
    matches!(name, "button" | "edit" | "static" | "combobox" | "listbox" | "scrollbar" | "#32770").then(|| Class { wndproc: 0, name: name.to_string() })
}

fn destroy_window(p: &mut Process, h: u64) -> Result<(), Stop> {
    if !p.extra.win.windows.contains_key(&h) {
        return Ok(());
    }
    send(p, h, WM_DESTROY, 0, 0)?;
    send(p, h, WM_NCDESTROY, 0, 0)?;
    p.extra.win.windows.remove(&h);
    if p.extra.win.main_window == h {
        p.extra.win.main_window = 0;
    }
    Ok(())
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

/// Translate a browser input event into window messages. Kinds:
/// 0 key down (a=vk), 1 key up, 2 char (a=utf16), 3 mouse move (a=x,b=y), 4 button down (a=btn 0/1/2,b=x,c=y),
/// 5 button up, 6 wheel (a=delta, b=x, c=y), 7 resize (a=w,b=h), 8 focus (a=1/0), 9 close request.
pub fn input_event(p: &mut Process, kind: u32, a: i64, b: i64, c: i64) {
    let h = p.extra.win.main_window;
    if h == 0 {
        return;
    }
    let packed = |x: i64, y: i64| ((y as u64 & 0xffff) << 16) | (x as u64 & 0xffff);
    match kind {
        0 | 1 => {
            if p.extra.win.keys.len() < 256 {
                p.extra.win.keys.resize(256, 0);
            }
            p.extra.win.keys[(a & 0xff) as usize] = (kind == 0) as u8;
            post(p, h, if kind == 0 { WM_KEYDOWN } else { WM_KEYUP }, a as u64, if kind == 0 { 1 } else { 0xc000_0001 });
        }
        2 => post(p, h, WM_CHAR, a as u64, 1),
        3 => {
            p.extra.win.cursor = (a as i32, b as i32);
            post(p, h, WM_MOUSEMOVE, 0, packed(a, b));
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
            post(p, h, if kind == 4 { m_down } else { m_up }, 0, packed(b, c));
        }
        6 => post(p, h, WM_MOUSEWHEEL, ((a as u64) & 0xffff) << 16, packed(b, c)),
        7 => {
            if let Some(w) = p.extra.win.windows.get_mut(&h) {
                w.w = a as i32;
                w.h = b as i32;
            }
            post(p, h, WM_SIZE, 0, packed(a, b));
        }
        8 => post(p, h, if a != 0 { WM_SETFOCUS } else { WM_KILLFOCUS }, 0, 0),
        9 => post(p, h, WM_CLOSE, 0, 0),
        _ => {}
    }
}
