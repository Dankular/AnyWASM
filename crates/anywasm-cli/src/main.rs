//! `anywasm` command line tool.
//!
//! ```text
//! anywasm analyze <exe> [--json]          compatibility report (which imports are implemented)
//! anywasm run <exe> [--dir D] [--cwd GUEST_DIR] [--trace-imports|--trace-modules] [--max-steps N] [-- args...]
//! anywasm pack <exe> -o <out.awb> [--dir D] [--cwd GUEST_DIR] [--args "..."]
//! ```

use anywasm_pe::{canonical_dll, ImportName, Pe};
use anywasm_rt::hle;
use anywasm_rt::vfs::absolute;
use anywasm_rt::{Bundle, Process, Stop, Vfs};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct ImportStatus {
    dll: String,
    group: Option<&'static str>,
    bundled: bool,
    implemented: Vec<String>,
    missing: Vec<String>,
}

#[derive(Default)]
struct Report {
    machine: String,
    subsystem: String,
    is_dll: bool,
    is_dotnet: bool,
    image_base: u64,
    size_of_image: u32,
    has_tls: bool,
    imports: Vec<ImportStatus>,
    features: BTreeSet<&'static str>,
    blockers: Vec<String>,
}

fn feature_of(dll: &str) -> Option<&'static str> {
    Some(match dll {
        "d3d9" => "Direct3D 9",
        "d3d10" | "d3d10_1" | "d3d10core" => "Direct3D 10",
        "d3d11" => "Direct3D 11",
        "d3d12" => "Direct3D 12",
        "dxgi" => "DXGI",
        "ddraw" => "DirectDraw",
        "opengl32" => "OpenGL",
        "vulkan-1" => "Vulkan",
        "dsound" => "DirectSound",
        "winmm" => "Windows multimedia/audio",
        "xaudio2_9" | "xaudio2_8" | "xaudio2_7" => "XAudio2",
        "xinput1_4" | "xinput1_3" | "xinput9_1_0" => "XInput gamepad",
        "dinput8" | "dinput" => "DirectInput",
        "ws2_32" | "wsock32" | "wininet" | "winhttp" => "Networking",
        "user32" => "Win32 windows/messages (user32)",
        "gdi32" => "GDI drawing",
        _ => return None,
    })
}

fn blocker_of(dll: &str) -> Option<&'static str> {
    Some(match dll {
        d if d.starts_with("easyanticheat") => "Easy Anti-Cheat (kernel/driver based; cannot run)",
        d if d.starts_with("beclient") || d.starts_with("battleye") => {
            "BattlEye anti-cheat (cannot run)"
        }
        "steam_api64" | "steam_api" => "Steamworks (needs a steam_api replacement)",
        "winusb" | "setupapi" | "hid" => "Direct device/driver access",
        _ => return None,
    })
}

fn analyze(path: &Path, dir: Option<&Path>) -> Result<Report, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pe = Pe::parse(&data).map_err(|e| e.to_string())?;
    let mut r = Report {
        machine: match pe.machine {
            anywasm_pe::MACHINE_AMD64 => "x86-64".into(),
            anywasm_pe::MACHINE_I386 => "x86 (32-bit)".into(),
            anywasm_pe::MACHINE_ARM64 => "ARM64".into(),
            m => format!("unknown ({m:#x})"),
        },
        subsystem: match pe.subsystem {
            anywasm_pe::SUBSYSTEM_WINDOWS_GUI => "Windows GUI".into(),
            anywasm_pe::SUBSYSTEM_WINDOWS_CUI => "console".into(),
            s => format!("subsystem {s}"),
        },
        is_dll: pe.is_dll(),
        is_dotnet: pe.is_dotnet(),
        image_base: pe.image_base,
        size_of_image: pe.size_of_image,
        has_tls: pe.tls().map_err(|e| e.to_string())?.is_some(),
        ..Default::default()
    };
    if pe.machine != anywasm_pe::MACHINE_AMD64 || !pe.is_64 {
        r.blockers.push(format!(
            "{} images are not supported: only PE32+ x86-64",
            r.machine
        ));
    }
    if pe.is_dotnet() {
        r.blockers
            .push(".NET managed executable (needs a CLR, not a native Win64 runtime)".into());
    }
    let bundled: BTreeSet<String> = dir
        .map(|d| {
            std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(|s| canonical_dll(s)))
                .collect()
        })
        .unwrap_or_default();
    let mut by_dll: BTreeMap<String, ImportStatus> = BTreeMap::new();
    for imp in pe.all_imports().map_err(|e| e.to_string())? {
        let canon = canonical_dll(&imp.dll);
        let group = hle::group_of(&canon);
        let is_bundled = bundled.contains(&canon);
        let st = by_dll.entry(canon.clone()).or_insert_with(|| ImportStatus {
            dll: imp.dll.clone(),
            group,
            bundled: is_bundled,
            implemented: vec![],
            missing: vec![],
        });
        if let Some(f) = feature_of(&canon) {
            r.features.insert(f);
        }
        if let Some(b) = blocker_of(&canon) {
            r.blockers.push(b.to_string());
        }
        for e in &imp.entries {
            let name = match &e.name {
                ImportName::Name { name, .. } => name.clone(),
                ImportName::Ordinal(o) => format!("#{o}"),
            };
            let ok = is_bundled
                || group
                    .map(|g| {
                        hle::lookup(g, &name).is_some() || hle::data_symbol(g, &name).is_some()
                    })
                    .unwrap_or(false);
            if ok {
                st.implemented.push(name);
            } else {
                st.missing.push(name);
            }
        }
    }
    r.imports = by_dll.into_values().collect();
    Ok(r)
}

fn preflight_status(known_blockers: usize, missing_imports: usize) -> &'static str {
    if known_blockers != 0 {
        "blocked"
    } else if missing_imports != 0 {
        "review"
    } else {
        "no_known_blockers"
    }
}

fn json_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{08}' => quoted.push_str("\\b"),
            '\u{0c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            c if c <= '\u{1f}' => quoted.push_str(&format!("\\u{:04x}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::{json_quote, preflight_status};

    #[test]
    fn json_quote_escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(
            json_quote("quote\" slash\\ newline\n tab\t control\u{0001}"),
            "\"quote\\\" slash\\\\ newline\\n tab\\t control\\u0001\""
        );
    }

    #[test]
    fn preflight_status_does_not_treat_missing_imports_as_definitive_blockers() {
        assert_eq!(preflight_status(1, 2), "blocked");
        assert_eq!(preflight_status(0, 2), "review");
        assert_eq!(preflight_status(0, 0), "no_known_blockers");
    }
}

fn print_report(r: &Report, json: bool) {
    let (imp, miss): (usize, usize) = r.imports.iter().fold((0, 0), |a, i| {
        (a.0 + i.implemented.len(), a.1 + i.missing.len())
    });
    if json {
        let list = |v: &[String]| {
            format!(
                "[{}]",
                v.iter()
                    .map(|s| json_quote(s))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let imports = r
            .imports
            .iter()
            .map(|i| {
                format!(
                    "{{\"dll\":{},\"bundled\":{},\"implemented\":{},\"missing\":{}}}",
                    json_quote(&i.dll),
                    i.bundled,
                    list(&i.implemented),
                    list(&i.missing)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let feats = r
            .features
            .iter()
            .map(|f| json_quote(f))
            .collect::<Vec<_>>()
            .join(",");
        let blockers = list(&r.blockers);
        let mut definitive_blocker_items = r.blockers.clone();
        definitive_blocker_items.sort();
        definitive_blocker_items.dedup();
        let definitive_blockers = list(&definitive_blocker_items);
        let potentially_unused = r
            .imports
            .iter()
            .filter(|i| !i.missing.is_empty())
            .map(|i| {
                format!(
                    "{{\"dll\":{},\"symbols\":{}}}",
                    json_quote(&i.dll),
                    list(&i.missing)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let preflight_status = preflight_status(r.blockers.len(), miss);
        println!(
            "{{\"schemaVersion\":1,\"machine\":{},\"subsystem\":{},\"dll\":{},\"dotnet\":{},\"imageBase\":\"{:#x}\",\"implemented\":{imp},\"missing\":{miss},\"features\":[{feats}],\"blockers\":{blockers},\"imports\":[{imports}],\"preflight\":{{\"status\":{},\"staticOnly\":true,\"runtimeSuccessGuaranteed\":false,\"definitiveBlockers\":{definitive_blockers},\"potentiallyUnusedMissingImports\":[{potentially_unused}]}}}}",
            json_quote(&r.machine),
            json_quote(&r.subsystem),
            r.is_dll,
            r.is_dotnet,
            r.image_base,
            json_quote(preflight_status)
        );
        return;
    }
    println!("Machine     : {}", r.machine);
    println!(
        "Subsystem   : {}{}",
        r.subsystem,
        if r.is_dll { " (DLL)" } else { "" }
    );
    println!(
        "Image       : base {:#x}, {} KiB mapped{}",
        r.image_base,
        r.size_of_image / 1024,
        if r.has_tls { ", has TLS" } else { "" }
    );
    if !r.features.is_empty() {
        println!(
            "Uses        : {}",
            r.features.iter().copied().collect::<Vec<_>>().join(", ")
        );
    }
    for b in &r.blockers {
        println!("BLOCKER     : {b}");
    }
    println!("\nImports ({imp} implemented, {miss} missing):");
    for i in &r.imports {
        let tag = if i.bundled {
            "bundled DLL".to_string()
        } else {
            i.group
                .map(|g| format!("host:{g}"))
                .unwrap_or_else(|| "NO HOST LIBRARY".into())
        };
        println!(
            "  {:<34} [{tag}] {} ok, {} missing",
            i.dll,
            i.implemented.len(),
            i.missing.len()
        );
        for m in i.missing.iter().take(12) {
            println!("      - missing {m}");
        }
        if i.missing.len() > 12 {
            println!("      ... and {} more", i.missing.len() - 12);
        }
    }
    println!();
    if !r.blockers.is_empty() {
        println!(
            "Verdict: cannot run ({} blocker(s) above).",
            r.blockers.len()
        );
    } else if miss == 0 {
        println!("Verdict: every imported function has a host implementation (this does not prove the program will work).");
    } else {
        println!("Verdict: {miss} imported function(s) are not implemented; the program stops when it first calls one.");
    }
}

fn load_dir(vfs: &mut Vfs, dir: &Path) -> std::io::Result<()> {
    fn walk(vfs: &mut Vfs, root: &Path, cur: &Path) -> std::io::Result<()> {
        for e in std::fs::read_dir(cur)? {
            let e = e?;
            let p = e.path();
            if p.is_dir() {
                walk(vfs, root, &p)?;
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('/', "\\");
                vfs.insert(&rel, std::fs::read(&p)?);
            }
        }
        Ok(())
    }
    walk(vfs, dir, dir)
}

struct Opts {
    exe: PathBuf,
    dir: Option<PathBuf>,
    out: Option<PathBuf>,
    trace: bool,
    trace_modules: bool,
    json: bool,
    max_steps: u64,
    args: Vec<String>,
    cmdline: Option<String>,
    cwd: Option<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        exe: PathBuf::new(),
        dir: None,
        out: None,
        trace: false,
        trace_modules: false,
        json: false,
        max_steps: 20_000_000_000,
        args: vec![],
        cmdline: None,
        cwd: None,
    };
    let mut it = args.iter();
    let mut have_exe = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => o.dir = Some(it.next().ok_or("--dir needs a value")?.into()),
            "-o" | "--out" => o.out = Some(it.next().ok_or("-o needs a value")?.into()),
            "--trace-imports" => o.trace = true,
            "--trace-modules" => o.trace_modules = true,
            "--json" => o.json = true,
            "--max-steps" => {
                o.max_steps = it
                    .next()
                    .ok_or("--max-steps needs a value")?
                    .parse()
                    .map_err(|_| "bad --max-steps")?
            }
            "--args" => o.cmdline = Some(it.next().ok_or("--args needs a value")?.clone()),
            "--cwd" => o.cwd = Some(it.next().ok_or("--cwd needs a value")?.clone()),
            "--" => {
                o.args = it.cloned().collect();
                break;
            }
            s if !have_exe && !s.starts_with('-') => {
                o.exe = s.into();
                have_exe = true;
            }
            s => return Err(format!("unknown option {s}")),
        }
    }
    if !have_exe {
        return Err("missing <exe>".into());
    }
    Ok(o)
}

fn build_vfs(o: &Opts) -> Result<(Vfs, String), String> {
    let mut vfs = Vfs::new();
    if let Some(d) = &o.dir {
        load_dir(&mut vfs, d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let name = o
        .exe
        .file_name()
        .ok_or("bad exe path")?
        .to_string_lossy()
        .into_owned();
    if !vfs.exists(&name) {
        vfs.insert(
            &name,
            std::fs::read(&o.exe).map_err(|e| format!("{}: {e}", o.exe.display()))?,
        );
    }
    Ok((vfs, name))
}

fn command_line(name: &str, o: &Opts) -> String {
    if let Some(c) = &o.cmdline {
        return c.clone();
    }
    let mut s = format!("\"C:\\{name}\"");
    for a in &o.args {
        s.push(' ');
        if a.contains(' ') {
            s.push_str(&format!("\"{a}\""))
        } else {
            s.push_str(a)
        }
    }
    s
}

fn guest_cwd(o: &Opts, vfs: &Vfs) -> Result<Option<String>, String> {
    let Some(cwd) = &o.cwd else { return Ok(None) };
    if cwd.is_empty() {
        return Err("guest current directory cannot be empty".into());
    }
    let cwd = absolute(cwd, "C:\\");
    if !vfs.is_dir(&cwd) {
        return Err(format!(
            "guest current directory is not present in the VFS: {cwd}"
        ));
    }
    Ok(Some(cwd))
}

fn run(o: &Opts) -> Result<ExitCode, String> {
    let (vfs, name) = build_vfs(o)?;
    let cwd = guest_cwd(o, &vfs)?.unwrap_or_else(|| "C:\\".into());
    let cmd = command_line(&name, o);
    let mut p = Process::new(vfs);
    p.current_directory = cwd;
    p.trace_imports = o.trace;
    p.trace_modules_only = o.trace_modules;
    if let Err(error) = p.load_exe(&name, Some(&cmd)) {
        if o.trace || o.trace_modules {
            std::io::stderr().write_all(&p.take_stderr()).ok();
        }
        return Err(error);
    }
    if std::env::var_os("ANYWASM_GTA_STARTUP_PROBE_SKIP_EVENT15").is_some() {
        const BRANCH: u64 = 0x748851;
        let opcode = p
            .mem
            .read_u8(BRANCH)
            .map_err(|error| format!("startup probe could not read {BRANCH:#x}: {error:?}"))?;
        if opcode != 0x75 {
            return Err(format!(
                "startup probe expected JNE at {BRANCH:#x}, found {opcode:#04x}"
            ));
        }
        p.mem
            .write_force(BRANCH, &[0xeb])
            .map_err(|error| format!("startup probe could not patch {BRANCH:#x}: {error:?}"))?;
        eprintln!("[gta-startup-probe] patched event 0x15 rejection branch in guest memory");
    }
    let mut left = o.max_steps;
    let stop = loop {
        if left == 0 {
            break Stop::StepLimit;
        }
        // Guest callbacks cannot suspend at a budget boundary, so don't split the user's cap.
        let before = p.retired_instructions;
        let s = p.run(left);
        left = left.saturating_sub(p.retired_instructions.saturating_sub(before));
        std::io::stdout().write_all(&p.take_stdout()).ok();
        std::io::stderr().write_all(&p.take_stderr()).ok();
        match s {
            Stop::StepLimit => break Stop::StepLimit,
            // No display attached: a presented frame needs no host work, keep going.
            Stop::Frame => {
                p.extra.gpu.buf.clear();
                if left == 0 {
                    break Stop::StepLimit;
                }
            }
            other => break other,
        }
    };
    std::io::stdout().flush().ok();
    match stop {
        Stop::Exit(c) => Ok(ExitCode::from((c & 0xff) as u8)),
        Stop::Unimplemented { dll, name } => {
            eprintln!("\nanywasm: stopped at unimplemented import {dll}!{name}");
            Ok(ExitCode::from(120))
        }
        Stop::Crash(m) => {
            eprintln!("\nanywasm: guest crashed: {m}");
            Ok(ExitCode::from(121))
        }
        Stop::StepLimit => {
            let module = p.module_for(p.cpu.rip).unwrap_or("?");
            eprintln!(
                "\nanywasm: step limit reached after {} retired instructions at {module}+{:#x} (RIP={:#x}, thread={})",
                p.retired_instructions,
                p.cpu.rip.saturating_sub(
                    p.modules
                        .iter()
                        .find(|m| m.hle_group.is_none() && m.name == module)
                        .map(|m| m.base)
                        .unwrap_or(0)
                ),
                p.cpu.rip,
                p.current_thread_id
            );
            Ok(ExitCode::from(122))
        }
        Stop::Frame | Stop::Idle => {
            eprintln!(
                "\nanywasm: program is waiting for window input (no display attached to the CLI)"
            );
            Ok(ExitCode::from(123))
        }
        Stop::ThreadExit(code) => {
            eprintln!("\nanywasm: internal thread exit escaped its worker context (code {code})");
            Ok(ExitCode::from(1))
        }
        Stop::GuestUnwind(_) => {
            eprintln!("\nanywasm: internal guest unwind escaped its exception dispatcher");
            Ok(ExitCode::from(1))
        }
    }
}

fn pack(o: &Opts) -> Result<(), String> {
    let (vfs, name) = build_vfs(o)?;
    let cwd = guest_cwd(o, &vfs)?;
    let out = o.out.clone().ok_or("pack needs -o <out.awb>")?;
    let mut files: Vec<(String, Vec<u8>)> = vfs
        .paths()
        .map(|p| (p.clone(), vfs.get(p).unwrap().clone()))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let b = Bundle {
        exe: name.to_ascii_lowercase(),
        cmdline: o.cmdline.clone().or_else(|| {
            if o.args.is_empty() {
                None
            } else {
                Some(command_line(&name, o))
            }
        }),
        cwd,
        files,
    };
    let bytes = b.to_bytes();
    std::fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "wrote {} ({} files, {} KiB)",
        out.display(),
        b.files.len(),
        bytes.len() / 1024
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage:\n  anywasm analyze <exe> [--dir D] [--json]\n  anywasm run <exe> [--dir D] [--cwd GUEST_DIR] [--trace-imports|--trace-modules] [--max-steps N] [-- args...]\n  anywasm pack <exe> -o <out.awb> [--dir D] [--cwd GUEST_DIR] [--args \"cmdline\"]";
    let Some(cmd) = args.first() else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let o = match parse(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("anywasm: {e}\n{usage}");
            return ExitCode::from(2);
        }
    };
    let res = match cmd.as_str() {
        "analyze" if o.cwd.is_some() => Err("--cwd is only supported by run and pack".into()),
        "analyze" => analyze(&o.exe, o.dir.as_deref()).map(|r| {
            print_report(&r, o.json);
            ExitCode::SUCCESS
        }),
        "run" => run(&o),
        "pack" => pack(&o).map(|_| ExitCode::SUCCESS),
        _ => Err(format!("unknown command {cmd}\n{usage}")),
    };
    match res {
        Ok(c) => c,
        Err(e) => {
            eprintln!("anywasm: {e}");
            ExitCode::from(1)
        }
    }
}
