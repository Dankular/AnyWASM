//! `anywasm` command line tool.
//!
//! ```text
//! anywasm analyze <exe> [--json]          import and x86 ABI compatibility report
//! anywasm run <exe> [--dir D] [--exclude PATH] [--cwd GUEST_DIR] [--hook SPEC]... [--trace-imports|--trace-modules] [--max-steps N] [-- args...]
//! anywasm pack <exe> -o <out.awb> [--dir D] [--exclude PATH] [--cwd GUEST_DIR] [--hook SPEC]... [--args "..."]
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
    abi_missing: Vec<String>,
}

#[derive(Default)]
struct BundledImage {
    names: BTreeSet<String>,
    ordinals: BTreeSet<u16>,
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
        "winusb" | "hid" => "Direct device/driver access",
        _ => return None,
    })
}

fn supported_pe_machine(machine: u16, is_64: bool) -> bool {
    matches!(
        (machine, is_64),
        (anywasm_pe::MACHINE_AMD64, true) | (anywasm_pe::MACHINE_I386, false)
    )
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
    if !supported_pe_machine(pe.machine, pe.is_64) {
        r.blockers.push(format!(
            "unsupported PE machine/magic combination: machine {:#x}, PE32+={}",
            pe.machine, pe.is_64
        ));
    }
    if pe.is_dotnet() {
        r.blockers
            .push(".NET managed executable (needs a CLR, not a native Win64 runtime)".into());
    }
    let bundled = bundled_images(dir)?;
    let mut by_dll: BTreeMap<String, ImportStatus> = BTreeMap::new();
    for imp in pe.all_imports().map_err(|e| e.to_string())? {
        let canon = canonical_dll(&imp.dll);
        let group = hle::group_of(&canon);
        let bundled_image = bundled.get(&canon);
        let is_bundled = bundled_image.is_some();
        let st = by_dll.entry(canon.clone()).or_insert_with(|| ImportStatus {
            dll: imp.dll.clone(),
            group,
            bundled: is_bundled,
            implemented: vec![],
            missing: vec![],
            abi_missing: vec![],
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
            let has_bundled_export = bundled_image
                .map(|image| bundled_exported(image, &e.name))
                .unwrap_or(false);
            let data_import = group
                .map(|g| hle::data_symbol(g, &name).is_some())
                .unwrap_or(false);
            let ok = if bundled_image.is_some() {
                has_bundled_export
            } else {
                group
                    .map(|g| {
                        hle::import_is_implemented(g, &name) || hle::data_symbol(g, &name).is_some()
                    })
                    .unwrap_or(false)
            };
            // Calls into a bundled guest DLL retain the guest binary's native ABI. HLE imports
            // cross the host boundary and need an explicit callee/caller cleanup rule.
            if missing_x86_abi(pe.machine, has_bundled_export, data_import, group, &name) {
                st.abi_missing.push(name.clone());
            }
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

fn missing_x86_abi(
    machine: u16,
    has_bundled_export: bool,
    data_import: bool,
    group: Option<&str>,
    name: &str,
) -> bool {
    machine == anywasm_pe::MACHINE_I386
        && !has_bundled_export
        && !data_import
        && group.is_some_and(|g| anywasm_rt::x86_import_cleanup_bytes(g, name).is_none())
}

fn bundled_images(dir: Option<&Path>) -> Result<BTreeMap<String, BundledImage>, String> {
    let Some(dir) = dir else {
        return Ok(BTreeMap::new());
    };
    fn collect_dlls(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        let mut entries = entries
            .map(|entry| entry.map_err(|error| format!("{}: {error}", directory.display())))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_dlls(&path, output)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
            {
                output.push(path);
            }
        }
        Ok(())
    }

    let mut dll_paths = Vec::new();
    collect_dlls(dir, &mut dll_paths)?;
    let mut images = BTreeMap::new();
    for path in dll_paths {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut image = BundledImage::default();
        if let Ok(pe) = Pe::parse(&bytes) {
            if let Ok(exports) = pe.exports() {
                for export in exports {
                    if let Some(name) = export.name {
                        image.names.insert(name);
                    }
                    image.ordinals.insert(export.ordinal);
                }
            }
        }
        images.entry(canonical_dll(file_name)).or_insert(image);
    }
    Ok(images)
}

fn bundled_exported(image: &BundledImage, name: &ImportName) -> bool {
    match name {
        ImportName::Name { name, .. } => image.names.contains(name),
        ImportName::Ordinal(ordinal) => image.ordinals.contains(ordinal),
    }
}

fn preflight_status(
    known_blockers: usize,
    missing_imports: usize,
    missing_abi: usize,
) -> &'static str {
    if known_blockers != 0 {
        "blocked"
    } else if missing_imports != 0 || missing_abi != 0 {
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
    use super::{
        blocker_of, bundled_exported, json_quote, missing_x86_abi, preflight_status,
        supported_pe_machine, BundledImage,
    };
    use anywasm_pe::ImportName;
    use std::collections::BTreeSet;

    #[test]
    fn json_quote_escapes_quotes_backslashes_and_control_characters() {
        assert_eq!(
            json_quote("quote\" slash\\ newline\n tab\t control\u{0001}"),
            "\"quote\\\" slash\\\\ newline\\n tab\\t control\\u0001\""
        );
    }

    #[test]
    fn preflight_status_does_not_treat_missing_imports_as_definitive_blockers() {
        assert_eq!(preflight_status(1, 2, 3), "blocked");
        assert_eq!(preflight_status(0, 2, 0), "review");
        assert_eq!(preflight_status(0, 0, 1), "review");
        assert_eq!(preflight_status(0, 0, 0), "no_known_blockers");
    }

    #[test]
    fn setupapi_imports_are_not_assumed_to_require_host_device_drivers() {
        assert_eq!(blocker_of("setupapi"), None);
        assert_eq!(blocker_of("winusb"), Some("Direct device/driver access"));
        assert_eq!(blocker_of("hid"), Some("Direct device/driver access"));
    }

    #[test]
    fn analyzer_accepts_supported_pe32_and_pe32_plus_machine_pairs() {
        assert!(supported_pe_machine(anywasm_pe::MACHINE_I386, false));
        assert!(supported_pe_machine(anywasm_pe::MACHINE_AMD64, true));
        assert!(!supported_pe_machine(anywasm_pe::MACHINE_I386, true));
        assert!(!supported_pe_machine(anywasm_pe::MACHINE_AMD64, false));
        assert!(!supported_pe_machine(anywasm_pe::MACHINE_ARM64, true));
    }

    #[test]
    fn x86_abi_audit_includes_imports_without_runtime_implementations() {
        assert!(!missing_x86_abi(
            anywasm_pe::MACHINE_I386,
            false,
            false,
            Some("user32"),
            "DialogBoxParamA"
        ));
        assert!(!missing_x86_abi(
            anywasm_pe::MACHINE_I386,
            false,
            false,
            Some("kernel32"),
            "CreatePipe"
        ));
        assert!(missing_x86_abi(
            anywasm_pe::MACHINE_I386,
            false,
            false,
            Some("not_a_real_api_set"),
            "UnknownApi"
        ));
        assert!(!missing_x86_abi(
            anywasm_pe::MACHINE_I386,
            true,
            false,
            Some("user32"),
            "DialogBoxParamA"
        ));
        assert!(!missing_x86_abi(
            anywasm_pe::MACHINE_I386,
            false,
            true,
            Some("user32"),
            "DialogBoxParamA"
        ));
    }

    #[test]
    fn bundled_dll_presence_does_not_cover_missing_name_or_ordinal_exports() {
        let image = BundledImage {
            names: ["Exported".to_owned()].into(),
            ordinals: BTreeSet::from([7]),
        };
        assert!(bundled_exported(
            &image,
            &ImportName::Name {
                hint: 0,
                name: "Exported".into()
            }
        ));
        assert!(!bundled_exported(
            &image,
            &ImportName::Name {
                hint: 0,
                name: "Missing".into()
            }
        ));
        assert!(bundled_exported(&image, &ImportName::Ordinal(7)));
        assert!(!bundled_exported(&image, &ImportName::Ordinal(8)));
    }
}

fn print_report(r: &Report, json: bool) {
    let (imp, miss, abi_miss): (usize, usize, usize) = r.imports.iter().fold((0, 0, 0), |a, i| {
        (
            a.0 + i.implemented.len(),
            a.1 + i.missing.len(),
            a.2 + i.abi_missing.len(),
        )
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
                    "{{\"dll\":{},\"bundled\":{},\"implemented\":{},\"missing\":{},\"abiMissing\":{}}}",
                    json_quote(&i.dll),
                    i.bundled,
                    list(&i.implemented),
                    list(&i.missing),
                    list(&i.abi_missing)
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
        let unresolved_abi = r
            .imports
            .iter()
            .filter(|i| !i.abi_missing.is_empty())
            .map(|i| {
                format!(
                    "{{\"dll\":{},\"symbols\":{}}}",
                    json_quote(&i.dll),
                    list(&i.abi_missing)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let preflight_status = preflight_status(r.blockers.len(), miss, abi_miss);
        println!(
            "{{\"schemaVersion\":1,\"machine\":{},\"subsystem\":{},\"dll\":{},\"dotnet\":{},\"imageBase\":\"{:#x}\",\"implemented\":{imp},\"missing\":{miss},\"abiMissing\":{abi_miss},\"features\":[{feats}],\"blockers\":{blockers},\"imports\":[{imports}],\"preflight\":{{\"status\":{},\"staticOnly\":true,\"runtimeSuccessGuaranteed\":false,\"definitiveBlockers\":{definitive_blockers},\"potentiallyUnusedMissingImports\":[{potentially_unused}],\"unresolvedX86AbiImports\":[{unresolved_abi}]}}}}",
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
    println!("\nImports ({imp} implemented, {miss} missing, {abi_miss} without x86 ABI cleanup):");
    for i in &r.imports {
        let tag = if i.bundled {
            "bundled DLL".to_string()
        } else {
            i.group
                .map(|g| format!("host:{g}"))
                .unwrap_or_else(|| "NO HOST LIBRARY".into())
        };
        println!(
            "  {:<34} [{tag}] {} ok, {} missing, {} ABI gaps",
            i.dll,
            i.implemented.len(),
            i.missing.len(),
            i.abi_missing.len()
        );
        for m in i.missing.iter().take(12) {
            println!("      - missing {m}");
        }
        if i.missing.len() > 12 {
            println!("      ... and {} more", i.missing.len() - 12);
        }
        for m in i.abi_missing.iter().take(12) {
            println!("      - missing x86 ABI cleanup for {m}");
        }
        if i.abi_missing.len() > 12 {
            println!(
                "      ... and {} more x86 ABI gaps",
                i.abi_missing.len() - 12
            );
        }
    }
    println!();
    if !r.blockers.is_empty() {
        println!(
            "Verdict: cannot run ({} blocker(s) above).",
            r.blockers.len()
        );
    } else if miss == 0 && abi_miss == 0 {
        println!("Verdict: every imported function resolves and has known x86 cleanup metadata (this does not prove the program will work).");
    } else {
        println!("Verdict: {miss} imported function(s) are not implemented and {abi_miss} imported function(s) lack x86 cleanup metadata.");
    }
}

fn load_dir(vfs: &mut Vfs, dir: &Path, excludes: &[String]) -> std::io::Result<()> {
    fn walk(vfs: &mut Vfs, root: &Path, cur: &Path, excludes: &[String]) -> std::io::Result<()> {
        let relative_dir = cur
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        if !relative_dir.is_empty()
            && excludes.iter().any(|excluded| {
                relative_dir == *excluded || relative_dir.starts_with(&format!("{excluded}\\"))
            })
        {
            return Ok(());
        }
        for e in std::fs::read_dir(cur)? {
            let e = e?;
            let p = e.path();
            if p.is_dir() {
                walk(vfs, root, &p, excludes)?;
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('/', "\\");
                if excludes.iter().any(|excluded| {
                    rel.eq_ignore_ascii_case(excluded)
                        || rel
                            .get(..excluded.len() + 1)
                            .is_some_and(|head| head.eq_ignore_ascii_case(&format!("{excluded}\\")))
                }) {
                    continue;
                }
                vfs.insert(&rel, std::fs::read(&p)?);
            }
        }
        Ok(())
    }
    let excludes = excludes
        .iter()
        .map(|path| {
            path.replace('/', "\\")
                .trim_matches('\\')
                .to_ascii_lowercase()
        })
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    walk(vfs, dir, dir, &excludes)
}

struct Opts {
    exe: PathBuf,
    dir: Option<PathBuf>,
    excludes: Vec<String>,
    out: Option<PathBuf>,
    trace: bool,
    trace_modules: bool,
    json: bool,
    max_steps: u64,
    args: Vec<String>,
    cmdline: Option<String>,
    cwd: Option<String>,
    hooks: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        exe: PathBuf::new(),
        dir: None,
        excludes: vec![],
        out: None,
        trace: false,
        trace_modules: false,
        json: false,
        max_steps: 20_000_000_000,
        args: vec![],
        cmdline: None,
        cwd: None,
        hooks: vec![],
    };
    let mut it = args.iter();
    let mut have_exe = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => o.dir = Some(it.next().ok_or("--dir needs a value")?.into()),
            "--exclude" => o
                .excludes
                .push(it.next().ok_or("--exclude needs a relative path")?.clone()),
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
            "--hook" => o
                .hooks
                .push(it.next().ok_or("--hook needs a value")?.clone()),
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
        load_dir(&mut vfs, d, &o.excludes).map_err(|e| format!("{}: {e}", d.display()))?;
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
    p.trace_gta_eventvol = std::env::var_os("ANYWASM_TRACE_GTA_EVENTVOL").is_some();
    if let Err(error) = p.load_exe(&name, Some(&cmd)) {
        if o.trace || o.trace_modules {
            std::io::stderr().write_all(&p.take_stderr()).ok();
        }
        return Err(error);
    }
    p.install_native_hooks(&o.hooks)?;
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
                p.advance_time_100ns(166_667);
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
        Stop::AbiUnimplemented { dll, name } => {
            eprintln!("\nanywasm: stopped at {dll}!{name}; x86 ABI signature is not verified");
            Ok(ExitCode::from(124))
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
        Stop::GuestExceptionTransfer => {
            eprintln!("\nanywasm: internal nested exception transfer escaped its dispatcher");
            Ok(ExitCode::from(1))
        }
        Stop::GuestCatch32(_) => {
            eprintln!(
                "\nanywasm: internal PE32 C++ catch transfer escaped its exception dispatcher"
            );
            Ok(ExitCode::from(1))
        }
        Stop::GuestUnwind32(_) => {
            eprintln!("\nanywasm: internal PE32 unwind transfer escaped its exception dispatcher");
            Ok(ExitCode::from(1))
        }
        Stop::GuestSehTransfer32(_) => {
            eprintln!("\nanywasm: internal PE32 SEH transfer escaped its exception dispatcher");
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
        hooks: o.hooks.clone(),
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
    let usage = "usage:\n  anywasm analyze <exe> [--dir D] [--json]\n  anywasm run <exe> [--dir D] [--exclude PATH] [--cwd GUEST_DIR] [--hook SPEC]... [--trace-imports|--trace-modules] [--max-steps N] [-- args...]\n  anywasm pack <exe> -o <out.awb> [--dir D] [--exclude PATH] [--cwd GUEST_DIR] [--hook SPEC]... [--args \"cmdline\"]";
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
