//! DbgHelp compatibility for guest module exports and process-local symbol sessions.

use crate::process::{HleFn, Process, Stop};
use std::collections::{BTreeMap, HashMap};

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_MOD_NOT_FOUND: u32 = 126;
const ERROR_INVALID_ADDRESS: u32 = 487;
const UNDNAME_NAME_ONLY: u32 = 0x1000;

#[derive(Default)]
pub struct DbgHelpState {
    sessions: HashMap<u32, SymbolSession>,
    options: u32,
}

#[derive(Default)]
struct SymbolSession {
    modules: BTreeMap<u64, SymbolModule>,
}

#[derive(Default)]
struct SymbolModule {
    size: u64,
    symbols: BTreeMap<u64, String>,
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "SymLoadModule64" => sym_load_module_64,
        "SymCleanup" => sym_cleanup,
        "UnDecorateSymbolName" => undecorate_symbol_name,
        "SymInitialize" => sym_initialize,
        "SymSetOptions" => sym_set_options,
        "SymGetLineFromAddr64" => sym_get_line_from_addr_64,
        "SymGetSymFromAddr64" => sym_get_sym_from_addr_64,
        _ => return None,
    })
}

fn sym_initialize(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0) as u32;
    let search_path_ptr = p.arg(1);
    let invade_process = p.arg(2) != 0;
    if handle == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if p.extra.dbghelp.sessions.contains_key(&handle) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let _search_path = if search_path_ptr == 0 {
        None
    } else {
        Some(p.read_cstr(search_path_ptr)?)
    };
    let modules = if invade_process {
        p.modules.iter().map(symbol_module_from_guest).collect()
    } else {
        Vec::new()
    };
    let mut session = SymbolSession::default();
    for (base, module) in modules {
        session.modules.insert(base, module);
    }
    p.extra.dbghelp.sessions.insert(handle, session);
    p.set_last_error(0);
    Ok(1)
}

fn sym_cleanup(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0) as u32;
    if p.extra.dbghelp.sessions.remove(&handle).is_some() {
        p.set_last_error(0);
        Ok(1)
    } else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        Ok(0)
    }
}

fn sym_set_options(p: &mut Process) -> Result<u64, Stop> {
    let new_options = p.arg(0) as u32;
    let previous = std::mem::replace(&mut p.extra.dbghelp.options, new_options);
    Ok(previous as u64)
}

fn sym_load_module_64(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0) as u32;
    let image_name_ptr = p.arg(2);
    let module_name_ptr = p.arg(3);
    let requested_base = (p.arg(5) as u32 as u64) << 32 | p.arg(4) as u32 as u64;
    let requested_size = p.arg(6) as u32 as u64;
    let image_name = if image_name_ptr == 0 {
        None
    } else {
        Some(p.read_cstr(image_name_ptr)?)
    };
    let module_name = if module_name_ptr == 0 {
        None
    } else {
        Some(p.read_cstr(module_name_ptr)?)
    };

    let Some(session) = p.extra.dbghelp.sessions.get(&handle) else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let candidate = p.modules.iter().find(|module| {
        (requested_base != 0 && module.base == requested_base)
            || image_name
                .as_deref()
                .is_some_and(|image| same_path(image, &module.path))
            || module_name
                .as_deref()
                .is_some_and(|name| same_module_name(name, &module.name))
    });
    let Some(module) = candidate else {
        p.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(0);
    };

    let base = if requested_base == 0 {
        module.base
    } else {
        requested_base
    };
    let size = if requested_size == 0 {
        module.size
    } else {
        requested_size
    };
    if session.modules.contains_key(&base) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let symbol_module = symbol_module_from_guest_at(module, base, size);
    p.extra
        .dbghelp
        .sessions
        .get_mut(&handle)
        .expect("session was checked above")
        .modules
        .insert(base, symbol_module);
    p.set_last_error(0);
    Ok(base)
}

fn sym_get_sym_from_addr_64(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0) as u32;
    let address = (p.arg(2) as u32 as u64) << 32 | p.arg(1) as u32 as u64;
    let displacement_ptr = p.arg(3);
    let symbol_ptr = p.arg(4);
    if symbol_ptr == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let max_name_length = p.rd32(symbol_ptr + 24)? as usize;
    if max_name_length == 0 || p.rd32(symbol_ptr)? < 28 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let found = p
        .extra
        .dbghelp
        .sessions
        .get(&handle)
        .and_then(|session| find_exported_symbol(session, address));
    let Some((symbol_address, name)) = found else {
        p.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(0);
    };

    let max_bytes = max_name_length.saturating_sub(1);
    let mut name_bytes = name.as_bytes()[..name.len().min(max_bytes)].to_vec();
    name_bytes.push(0);
    // IMAGEHLP_SYMBOL64 has 8-byte alignment for Address on x86: SizeOfStruct at 0,
    // Address at 8, Size/Flags/MaxNameLength at 16/20/24, and Name at 28.
    p.wr64(symbol_ptr + 8, symbol_address)?;
    p.wr32(symbol_ptr + 16, 0)?; // Export table does not carry an exact symbol size.
    p.wr32(symbol_ptr + 20, 0)?;
    p.write_bytes(symbol_ptr + 28, &name_bytes)?;
    if displacement_ptr != 0 {
        p.wr64(displacement_ptr, address - symbol_address)?;
    }
    p.set_last_error(0);
    Ok(1)
}

fn sym_get_line_from_addr_64(p: &mut Process) -> Result<u64, Stop> {
    let handle = p.arg(0) as u32;
    let address = (p.arg(2) as u32 as u64) << 32 | p.arg(1) as u32 as u64;
    let displacement_ptr = p.arg(3);
    let line_ptr = p.arg(4);
    if line_ptr == 0 || p.rd32(line_ptr)? < 24 || !p.extra.dbghelp.sessions.contains_key(&handle) {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    // The loaded guest PE model currently exposes exports and exception tables but no PDB or
    // CodeView line records. Returning a line number here would invent source information.
    let _ = (address, displacement_ptr);
    p.set_last_error(ERROR_INVALID_ADDRESS);
    Ok(0)
}

fn undecorate_symbol_name(p: &mut Process) -> Result<u64, Stop> {
    let input_ptr = p.arg(0);
    let output_ptr = p.arg(1);
    let output_capacity = p.arg(2) as usize;
    let flags = p.arg(3) as u32;
    if input_ptr == 0 || output_ptr == 0 || output_capacity == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let input = p.read_cstr(input_ptr)?;
    let undecorated = match undecorate_name_only(&input, flags) {
        Some(name) => name,
        None => {
            return Err(Stop::Unimplemented {
                dll: "dbghelp".to_owned(),
                name: format!("UnDecorateSymbolName flags={flags:#x} input={input:?}"),
            });
        }
    };
    let copy_len = undecorated.len().min(output_capacity - 1);
    let mut output = undecorated.as_bytes()[..copy_len].to_vec();
    output.push(0);
    p.write_bytes(output_ptr, &output)?;
    p.set_last_error(0);
    Ok(copy_len as u64)
}

fn undecorate_name_only(name: &str, flags: u32) -> Option<String> {
    if flags & !UNDNAME_NAME_ONLY != 0 {
        return None;
    }
    if flags & UNDNAME_NAME_ONLY == 0 {
        return (!name.starts_with('?')).then(|| name.to_owned());
    }
    if let Some(cpp) = name.strip_prefix('?') {
        if cpp.starts_with('?') {
            return None;
        }
        let identifier = cpp.split('@').next()?;
        return (!identifier.is_empty()).then(|| identifier.to_owned());
    }

    let undecorated = name.strip_prefix('_').unwrap_or(name);
    let undecorated = if let Some((base, suffix)) = undecorated.rsplit_once('@') {
        if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            base
        } else {
            undecorated
        }
    } else {
        undecorated
    };
    (!undecorated.is_empty()).then(|| undecorated.to_owned())
}

fn symbol_module_from_guest(module: &crate::process::Module) -> (u64, SymbolModule) {
    let symbol_module = symbol_module_from_guest_at(module, module.base, module.size);
    (module.base, symbol_module)
}

fn symbol_module_from_guest_at(
    module: &crate::process::Module,
    base: u64,
    size: u64,
) -> SymbolModule {
    let symbols = module
        .exports
        .iter()
        .filter_map(|(name, address)| {
            let relative = address.checked_sub(module.base)?;
            Some((base.checked_add(relative)?, name.clone()))
        })
        .collect();
    SymbolModule { size, symbols }
}

fn find_exported_symbol(session: &SymbolSession, address: u64) -> Option<(u64, String)> {
    let (module_base, module) = session.modules.range(..=address).next_back()?;
    if address >= module_base.checked_add(module.size)? {
        return None;
    }
    let (symbol_address, name) = module.symbols.range(..=address).next_back()?;
    Some((*symbol_address, name.clone()))
}

fn same_path(left: &str, right: &str) -> bool {
    normalize_path(left).eq_ignore_ascii_case(&normalize_path(right))
}

fn same_module_name(left: &str, right: &str) -> bool {
    module_basename(left).eq_ignore_ascii_case(&module_basename(right))
}

fn normalize_path(path: &str) -> String {
    path.replace('/', "\\")
}

fn module_basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::{lookup, UNDNAME_NAME_ONLY};
    use crate::process::{Module, Process, Stop, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x20000;
    const STACK: u64 = BASE + 0x800;
    const SESSION: u32 = 0x1234;

    fn process() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.modules.push(Module {
            name: "fixture.dll".to_owned(),
            path: "C:\\fixture.dll".to_owned(),
            base: 0x400000,
            size: 0x10000,
            entry: 0x401000,
            is_dll: true,
            tls_callbacks: Vec::new(),
            exports: [("Exported".to_owned(), 0x401000)].into(),
            ordinals: [(1, 0x401000)].into(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: 0,
            resource_size: 0,
            hle_group: None,
        });
        p
    }

    fn call(p: &mut Process, name: &str, args: &[u32]) -> Result<u64, Stop> {
        for (index, arg) in args.iter().enumerate() {
            p.mem.write_u32(STACK + 4 + 4 * index as u64, *arg).unwrap();
        }
        lookup(name).expect("import should resolve")(p)
    }

    #[test]
    fn all_sims3_dbghelp_imports_resolve_with_generated_x86_abi() {
        let names = [
            "SymLoadModule64",
            "SymCleanup",
            "UnDecorateSymbolName",
            "SymInitialize",
            "SymSetOptions",
            "SymGetLineFromAddr64",
            "SymGetSymFromAddr64",
        ];
        assert_eq!(crate::hle::group_of("dbghelp"), Some("dbghelp"));
        for name in names {
            assert!(lookup(name).is_some(), "dbghelp!{name} should resolve");
            assert!(
                crate::hle::lookup("dbghelp", name).is_some(),
                "dbghelp!{name} should be reachable through the DLL resolver"
            );
            assert!(
                crate::x86_abi_generated::cleanup_bytes("dbghelp", name).is_some(),
                "dbghelp!{name} should have x86 cleanup metadata"
            );
        }
    }

    #[test]
    fn symbol_sessions_load_guest_exports_and_return_x86_displacements() {
        let mut p = process();
        assert_eq!(call(&mut p, "SymInitialize", &[SESSION, 0, 1]), Ok(1));

        let displacement = BASE + 0x100;
        let symbol = BASE + 0x200;
        p.wr32(symbol, 32).unwrap();
        p.wr32(symbol + 24, 32).unwrap();
        assert_eq!(
            call(
                &mut p,
                "SymGetSymFromAddr64",
                &[SESSION, 0x401005, 0, displacement as u32, symbol as u32]
            ),
            Ok(1)
        );
        assert_eq!(p.rd64(symbol + 8).unwrap(), 0x401000);
        assert_eq!(p.rd64(displacement).unwrap(), 5);
        assert_eq!(p.read_cstr(symbol + 28).unwrap(), "Exported");

        assert_eq!(call(&mut p, "SymCleanup", &[SESSION]), Ok(1));
        assert_eq!(
            call(
                &mut p,
                "SymGetSymFromAddr64",
                &[SESSION, 0x401005, 0, displacement as u32, symbol as u32]
            ),
            Ok(0)
        );
    }

    #[test]
    fn sym_load_module64_uses_x86_low_high_base_slots_and_explicit_module_match() {
        let mut p = process();
        let image = BASE + 0x300;
        let name = BASE + 0x380;
        p.write_cstr(image, "C:\\fixture.dll").unwrap();
        p.write_cstr(name, "fixture.dll").unwrap();
        assert_eq!(call(&mut p, "SymInitialize", &[SESSION, 0, 0]), Ok(1));

        assert_eq!(
            call(
                &mut p,
                "SymLoadModule64",
                &[SESSION, 0, image as u32, name as u32, 0x402000, 0, 0x10000]
            ),
            Ok(0x402000)
        );
        let symbol = BASE + 0x500;
        p.wr32(symbol, 32).unwrap();
        p.wr32(symbol + 24, 32).unwrap();
        assert_eq!(
            call(
                &mut p,
                "SymGetSymFromAddr64",
                &[SESSION, 0x403002, 0, 0, symbol as u32]
            ),
            Ok(1)
        );
        assert_eq!(p.rd64(symbol + 8).unwrap(), 0x403000);
        assert_eq!(p.read_cstr(symbol + 28).unwrap(), "Exported");
    }

    #[test]
    fn undecorate_symbol_name_supports_name_only_and_rejects_unmodeled_flags() {
        let mut p = process();
        let input = BASE + 0x100;
        let output = BASE + 0x200;
        p.write_cstr(input, "?Render@@YAXXZ").unwrap();
        assert_eq!(
            call(
                &mut p,
                "UnDecorateSymbolName",
                &[input as u32, output as u32, 64, UNDNAME_NAME_ONLY]
            ),
            Ok(6)
        );
        assert_eq!(p.read_cstr(output).unwrap(), "Render");

        assert!(matches!(
            call(
                &mut p,
                "UnDecorateSymbolName",
                &[input as u32, output as u32, 64, 0]
            ),
            Err(Stop::Unimplemented { dll, .. }) if dll == "dbghelp"
        ));
    }

    #[test]
    fn missing_source_line_tables_fail_instead_of_inventing_a_line() {
        let mut p = process();
        let line = BASE + 0x300;
        assert_eq!(call(&mut p, "SymInitialize", &[SESSION, 0, 1]), Ok(1));
        p.wr32(line, 24).unwrap();
        assert_eq!(
            call(
                &mut p,
                "SymGetLineFromAddr64",
                &[SESSION, 0x401005, 0, 0, line as u32]
            ),
            Ok(0)
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), 487);
    }

    #[test]
    fn sym_set_options_returns_the_previous_process_options() {
        let mut p = process();
        assert_eq!(call(&mut p, "SymSetOptions", &[0x10]), Ok(0));
        assert_eq!(call(&mut p, "SymSetOptions", &[0x200]), Ok(0x10));
    }
}
