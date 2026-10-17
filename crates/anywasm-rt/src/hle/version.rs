//! VERSION.dll APIs backed by the loaded PE image's RT_VERSION resource.

use crate::process::{HleFn, Module, Process, Stop};
use anywasm_cpu::mem::PROT_RW;
use std::collections::HashMap;

const RT_VERSION: u16 = 16;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_BAD_FORMAT: u32 = 11;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_RESOURCE_DATA_NOT_FOUND: u32 = 1812;
const VS_FIXEDFILEINFO_SIGNATURE: u32 = 0xFEEF_04BD;
const VS_FIXEDFILEINFO_SIZE: usize = 13 * 4;

#[derive(Default)]
pub struct VersionState {
    /// Length of the RT_VERSION bytes copied to each successful GetFileVersionInfoA block.
    pub blocks: HashMap<u64, usize>,
    /// ANSI query results are guest allocations retained for the process lifetime.
    pub ansi_strings: HashMap<(u64, String), AnsiString>,
}

#[derive(Clone)]
pub struct AnsiString {
    pub address: u64,
    pub length: u32,
    pub bytes: Vec<u8>,
}

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "GetFileVersionInfoSizeA" => Some(get_file_version_info_size_a),
        "GetFileVersionInfoA" => Some(get_file_version_info_a),
        "VerQueryValueA" => Some(ver_query_value_a),
        _ => None,
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn find_module<'a>(
    modules: &'a [Module],
    filename: &str,
    current_directory: &str,
) -> Option<&'a Module> {
    let requested = normalize_path(filename);
    let absolute = normalize_path(&crate::vfs::absolute(filename, current_directory));
    let has_directory = filename.contains(['\\', '/']);
    modules.iter().find(|module| {
        let module_path = normalize_path(&module.path);
        let module_name = normalize_path(&module.name);
        if has_directory {
            module_path == requested || module_path == absolute
        } else {
            module_name == requested
                || normalize_path(basename(&module_path)) == requested
                || normalize_path(basename(&module_name)) == requested
        }
    })
}

fn version_resource(p: &Process, filename: &str) -> Result<Option<Vec<u8>>, u32> {
    let Some(module) = find_module(&p.modules, filename, &p.current_directory) else {
        return Err(ERROR_FILE_NOT_FOUND);
    };
    let Some((address, length)) =
        super::kernel32::resource_payload_by_id(p, module.base, RT_VERSION, 1)
            .map_err(|_| ERROR_BAD_FORMAT)?
    else {
        return Ok(None);
    };
    let bytes = p
        .read_bytes(address, length as usize)
        .map_err(|_| ERROR_BAD_FORMAT)?;
    validate_version_root(&bytes).map_err(|_| ERROR_BAD_FORMAT)?;
    Ok(Some(bytes))
}

fn get_file_version_info_size_a(p: &mut Process) -> Result<u64, Stop> {
    let filename_ptr = p.arg(0);
    let handle_out = p.arg(1);
    if handle_out != 0 {
        p.wr32(handle_out, 0)?;
    }
    if filename_ptr == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let filename = p.read_cstr(filename_ptr)?;
    match version_resource(p, &filename) {
        Ok(Some(bytes)) if bytes.len() <= u32::MAX as usize => Ok(bytes.len() as u64),
        Ok(Some(_)) => {
            p.set_last_error(ERROR_BAD_FORMAT);
            Ok(0)
        }
        Ok(None) => {
            p.set_last_error(ERROR_RESOURCE_DATA_NOT_FOUND);
            Ok(0)
        }
        Err(error) => {
            p.set_last_error(error);
            Ok(0)
        }
    }
}

fn get_file_version_info_a(p: &mut Process) -> Result<u64, Stop> {
    let (filename_ptr, _ignored_handle, requested_len, output) =
        (p.arg(0), p.arg(1), p.arg(2) as u32 as usize, p.arg(3));
    if output != 0 {
        p.extra.version.blocks.remove(&output);
        p.extra
            .version
            .ansi_strings
            .retain(|(block, _), _| *block != output);
    }
    if filename_ptr == 0 || output == 0 || requested_len == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let filename = p.read_cstr(filename_ptr)?;
    let bytes = match version_resource(p, &filename) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            p.set_last_error(ERROR_RESOURCE_DATA_NOT_FOUND);
            return Ok(0);
        }
        Err(error) => {
            p.set_last_error(error);
            return Ok(0);
        }
    };

    // Windows permits a short caller buffer and copies the available prefix.
    let copied_len = requested_len.min(bytes.len());
    p.write_bytes(output, &bytes[..copied_len])?;
    p.extra.version.blocks.insert(output, copied_len);
    Ok(1)
}

#[derive(Debug)]
struct VersionNode {
    key: String,
    value_type: u16,
    value_length: usize,
    start: usize,
    end: usize,
    value_start: usize,
    value_end: usize,
    children_start: usize,
}

#[derive(Debug)]
struct MalformedVersion;

fn align4(offset: usize) -> Option<usize> {
    offset.checked_add(3).map(|value| value & !3)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, MalformedVersion> {
    let raw = bytes
        .get(offset..offset.checked_add(2).ok_or(MalformedVersion)?)
        .ok_or(MalformedVersion)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn parse_node(
    bytes: &[u8],
    start: usize,
    parent_end: usize,
) -> Result<VersionNode, MalformedVersion> {
    let length = read_u16(bytes, start)? as usize;
    let value_length = read_u16(bytes, start + 2)? as usize;
    let value_type = read_u16(bytes, start + 4)?;
    let end = start.checked_add(length).ok_or(MalformedVersion)?;
    if length < 8 || end > parent_end || end > bytes.len() {
        return Err(MalformedVersion);
    }

    let mut key_end = start + 6;
    let mut key_units = Vec::new();
    loop {
        let unit = read_u16(bytes, key_end)?;
        key_end = key_end.checked_add(2).ok_or(MalformedVersion)?;
        if unit == 0 {
            break;
        }
        key_units.push(unit);
        if key_end > end {
            return Err(MalformedVersion);
        }
    }
    if key_end > end {
        return Err(MalformedVersion);
    }
    let key = String::from_utf16(&key_units).map_err(|_| MalformedVersion)?;
    let value_start = align4(key_end).ok_or(MalformedVersion)?;
    let value_bytes = if value_type == 1 {
        value_length.checked_mul(2).ok_or(MalformedVersion)?
    } else {
        value_length
    };
    let value_end = value_start
        .checked_add(value_bytes)
        .ok_or(MalformedVersion)?;
    if value_start > end || value_end > end {
        return Err(MalformedVersion);
    }
    let children_start = align4(value_end).ok_or(MalformedVersion)?;
    Ok(VersionNode {
        key,
        value_type,
        value_length,
        start,
        end,
        value_start,
        value_end,
        children_start: children_start.min(end),
    })
}

fn child_nodes(bytes: &[u8], parent: &VersionNode) -> Result<Vec<VersionNode>, MalformedVersion> {
    let mut children = Vec::new();
    let mut offset = parent.children_start;
    while offset < parent.end {
        if bytes
            .get(offset..parent.end)
            .ok_or(MalformedVersion)?
            .iter()
            .all(|byte| *byte == 0)
        {
            break;
        }
        let node = parse_node(bytes, offset, parent.end)?;
        let next = align4(node.end).ok_or(MalformedVersion)?;
        children.push(node);
        if next <= offset {
            return Err(MalformedVersion);
        }
        offset = next;
    }
    Ok(children)
}

fn validate_children_recursive(
    bytes: &[u8],
    parent: &VersionNode,
    depth: usize,
) -> Result<(), MalformedVersion> {
    if depth > 16 {
        return Err(MalformedVersion);
    }
    for child in child_nodes(bytes, parent)? {
        validate_children_recursive(bytes, &child, depth + 1)?;
    }
    Ok(())
}

fn parse_root(bytes: &[u8]) -> Result<VersionNode, MalformedVersion> {
    let root = parse_node(bytes, 0, bytes.len())?;
    if root.key != "VS_VERSION_INFO"
        || root.value_type != 0
        || root.value_length != VS_FIXEDFILEINFO_SIZE
        || root.value_end - root.value_start != VS_FIXEDFILEINFO_SIZE
        || read_u16(bytes, root.start)? as usize != root.end - root.start
    {
        return Err(MalformedVersion);
    }
    let signature = u32::from_le_bytes(
        bytes[root.value_start..root.value_start + 4]
            .try_into()
            .map_err(|_| MalformedVersion)?,
    );
    if signature != VS_FIXEDFILEINFO_SIGNATURE {
        return Err(MalformedVersion);
    }
    validate_children_recursive(bytes, &root, 0)?;
    Ok(root)
}

fn validate_version_root(bytes: &[u8]) -> Result<(), MalformedVersion> {
    parse_root(bytes).map(|_| ())
}

fn find_child(
    bytes: &[u8],
    parent: &VersionNode,
    key: &str,
) -> Result<Option<VersionNode>, MalformedVersion> {
    Ok(child_nodes(bytes, parent)?
        .into_iter()
        .find(|child| child.key.eq_ignore_ascii_case(key)))
}

fn query_root(bytes: &[u8]) -> Result<Option<(usize, u32)>, MalformedVersion> {
    let root = parse_root(bytes)?;
    Ok(Some((root.value_start, VS_FIXEDFILEINFO_SIZE as u32)))
}

fn query_translation(bytes: &[u8]) -> Result<Option<(usize, u32)>, MalformedVersion> {
    let root = parse_root(bytes)?;
    let Some(var_file_info) = find_child(bytes, &root, "VarFileInfo")? else {
        return Ok(None);
    };
    let Some(translation) = find_child(bytes, &var_file_info, "Translation")? else {
        return Ok(None);
    };
    if translation.value_type != 0 || translation.value_length % 4 != 0 {
        return Err(MalformedVersion);
    }
    Ok(Some((
        translation.value_start,
        translation.value_length as u32,
    )))
}

fn query_string_units(
    bytes: &[u8],
    language_codepage: &str,
    string_name: &str,
) -> Result<Option<Vec<u16>>, MalformedVersion> {
    if language_codepage.len() != 8 || !language_codepage.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let root = parse_root(bytes)?;
    let Some(string_file_info) = find_child(bytes, &root, "StringFileInfo")? else {
        return Ok(None);
    };
    let Some(table) = find_child(bytes, &string_file_info, language_codepage)? else {
        return Ok(None);
    };
    let Some(string) = find_child(bytes, &table, string_name)? else {
        return Ok(None);
    };
    if string.value_type != 1 {
        return Err(MalformedVersion);
    }
    let mut units = Vec::with_capacity(string.value_length);
    let mut offset = string.value_start;
    while offset < string.value_end {
        units.push(read_u16(bytes, offset)?);
        offset += 2;
    }
    if units.last() != Some(&0) {
        units.push(0);
    }
    Ok(Some(units))
}

fn clear_query_outputs(p: &mut Process, pointer_out: u64, length_out: u64) -> Result<(), Stop> {
    if pointer_out != 0 {
        p.wrptr(pointer_out, 0)?;
    }
    if length_out != 0 {
        p.wr32(length_out, 0)?;
    }
    Ok(())
}

fn ver_query_value_a(p: &mut Process) -> Result<u64, Stop> {
    let (block, path_ptr, pointer_out, length_out) = (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if pointer_out == 0 || length_out == 0 {
        clear_query_outputs(p, pointer_out, length_out)?;
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    clear_query_outputs(p, pointer_out, length_out)?;
    if block == 0 || path_ptr == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let Some(&block_len) = p.extra.version.blocks.get(&block) else {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let path = p.read_cstr(path_ptr)?;
    let block_bytes = p.read_bytes(block, block_len)?;
    let query = path.to_ascii_lowercase();

    if path == "\\" {
        return match query_root(&block_bytes) {
            Ok(Some((offset, length))) if offset + length as usize <= block_len => {
                p.wrptr(pointer_out, block + offset as u64)?;
                p.wr32(length_out, length)?;
                Ok(1)
            }
            Ok(_) => Ok(0),
            Err(_) => {
                p.set_last_error(ERROR_BAD_FORMAT);
                Ok(0)
            }
        };
    }
    if path.eq_ignore_ascii_case("\\VarFileInfo\\Translation") {
        return match query_translation(&block_bytes) {
            Ok(Some((offset, length))) if offset + length as usize <= block_len => {
                p.wrptr(pointer_out, block + offset as u64)?;
                p.wr32(length_out, length)?;
                Ok(1)
            }
            Ok(_) => Ok(0),
            Err(_) => {
                p.set_last_error(ERROR_BAD_FORMAT);
                Ok(0)
            }
        };
    }

    let components: Vec<&str> = path.split('\\').filter(|s| !s.is_empty()).collect();
    if components.len() != 3 || !components[0].eq_ignore_ascii_case("StringFileInfo") {
        return Ok(0);
    }
    match query_string_units(&block_bytes, components[1], components[2]) {
        Ok(Some(units)) => {
            let mut bytes = super::kernel32::cp_encode(1252, &units);
            if bytes.last() != Some(&0) {
                bytes.push(0);
            }
            let key = (block, query);
            let (address, length) = if let Some(cached) = p.extra.version.ansi_strings.get(&key) {
                (cached.address, cached.length)
            } else {
                let Some(address) = p.try_alloc_virtual(bytes.len() as u64, PROT_RW) else {
                    p.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
                    return Ok(0);
                };
                p.write_bytes(address, &bytes)?;
                let Ok(length) = u32::try_from(bytes.len()) else {
                    p.set_last_error(ERROR_BAD_FORMAT);
                    return Ok(0);
                };
                p.extra.version.ansi_strings.insert(
                    key,
                    AnsiString {
                        address,
                        length,
                        bytes,
                    },
                );
                (address, length)
            };
            p.wrptr(pointer_out, address)?;
            p.wr32(length_out, length)?;
            Ok(1)
        }
        Ok(None) => Ok(0),
        Err(_) => {
            p.set_last_error(ERROR_BAD_FORMAT);
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        lookup, ERROR_BAD_FORMAT, ERROR_FILE_NOT_FOUND, RT_VERSION, VS_FIXEDFILEINFO_SIGNATURE,
    };
    use crate::process::{Module, Process, Stop, TEB_ADDR};
    use crate::vfs::Vfs;
    use anywasm_cpu::cpu::{CpuMode, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const LOW: u64 = 0x30000;
    const STACK: u64 = LOW + 0x800;
    const MODULE: u64 = 0x0040_0000;
    const RESOURCE_RVA: u32 = 0x1000;

    fn pad4(bytes: &mut Vec<u8>) {
        while bytes.len() & 3 != 0 {
            bytes.push(0);
        }
    }

    fn node(
        key: &str,
        value_type: u16,
        value_length: u16,
        value: &[u8],
        children: &[u8],
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; 6];
        for unit in key.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        pad4(&mut bytes);
        bytes.extend_from_slice(value);
        pad4(&mut bytes);
        bytes.extend_from_slice(children);
        let length = u16::try_from(bytes.len()).unwrap();
        bytes[0..2].copy_from_slice(&length.to_le_bytes());
        bytes[2..4].copy_from_slice(&value_length.to_le_bytes());
        bytes[4..6].copy_from_slice(&value_type.to_le_bytes());
        bytes
    }

    fn children(nodes: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        let mut bytes = Vec::new();
        for mut child in nodes {
            pad4(&mut bytes);
            bytes.append(&mut child);
        }
        bytes
    }

    fn version_fixture() -> Vec<u8> {
        let fixed_info: Vec<u8> = [
            0xFEEF_04BDu32,
            0x0001_0000,
            0x0001_0002,
            0x0003_0004,
            0x0001_0002,
            0x0003_0004,
            0x3f,
            0,
            0x0004_0004,
            1,
            0,
            0,
            0,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let translation = [0x0409u16, 0x04b0]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let string: Vec<u8> = "Sims 3\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let string_node = node("FileDescription", 1, 7, &string, &[]);
        let table = node("040904B0", 1, 0, &[], &children([string_node]));
        let string_file_info = node("StringFileInfo", 1, 0, &[], &children([table]));
        let translation_node = node("Translation", 0, 4, &translation, &[]);
        let var_file_info = node("VarFileInfo", 1, 0, &[], &children([translation_node]));
        node(
            "VS_VERSION_INFO",
            0,
            52,
            &fixed_info,
            &children([string_file_info, var_file_info]),
        )
    }

    fn process_with_version_resource(bytes: &[u8]) -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(LOW, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        assert!(p.mem.map(MODULE, 0x4000, PROT_RW));
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.vmem_next = 0x1000_0000;
        p.modules.push(Module {
            name: "TS3W.exe".into(),
            path: "D:\\The Sims 3\\Game\\Bin\\TS3W.exe".into(),
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
            resource_size: 0x1000,
            hle_group: None,
        });

        // IMAGE_RESOURCE_DIRECTORY -> RT_VERSION (16) -> resource ID 1 -> language 0x0409.
        let root = MODULE + RESOURCE_RVA as u64;
        p.wr16(root + 14, 1).unwrap();
        p.wr32(root + 16, RT_VERSION as u32).unwrap();
        p.wr32(root + 20, 0x8000_0020).unwrap();
        let type_dir = root + 0x20;
        p.wr16(type_dir + 14, 1).unwrap();
        p.wr32(type_dir + 16, 1).unwrap();
        p.wr32(type_dir + 20, 0x8000_0040).unwrap();
        let name_dir = root + 0x40;
        p.wr16(name_dir + 14, 1).unwrap();
        p.wr32(name_dir + 16, 0x0409).unwrap();
        p.wr32(name_dir + 20, 0x60).unwrap();
        let data_entry = root + 0x60;
        p.wr32(data_entry, 0x2000).unwrap();
        p.wr32(data_entry + 4, bytes.len() as u32).unwrap();
        p.write_bytes(MODULE + 0x2000, bytes).unwrap();
        p
    }

    fn call_x86(p: &mut Process, name: &str, args: &[u32]) -> Result<u64, Stop> {
        p.cpu.mode = CpuMode::X86_32;
        p.cpu.gpr[RSP] = STACK;
        p.wr32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            p.wr32(STACK + 4 + index as u64 * 4, *value).unwrap();
        }
        lookup(name).expect("VERSION.dll API should resolve")(p)
    }

    fn write_query_path(p: &mut Process, path: &str) -> u32 {
        let address = LOW + 0x200;
        p.write_cstr(address, path).unwrap();
        address as u32
    }

    fn load_version_block(p: &mut Process, bytes: &[u8]) -> u32 {
        let filename = LOW + 0x100;
        p.write_cstr(filename, "D:\\The Sims 3\\Game\\Bin\\TS3W.exe")
            .unwrap();
        let block = LOW + 0x400;
        assert_eq!(
            call_x86(
                p,
                "GetFileVersionInfoA",
                &[filename as u32, 0, bytes.len() as u32, block as u32],
            ),
            Ok(1)
        );
        block as u32
    }

    fn query(p: &mut Process, block: u32, path: &str) -> (u64, u32, u64) {
        let path = write_query_path(p, path);
        let pointer_out = LOW + 0x280;
        let length_out = LOW + 0x288;
        let result = call_x86(
            p,
            "VerQueryValueA",
            &[block, path, pointer_out as u32, length_out as u32],
        )
        .unwrap();
        (
            result,
            p.rd32(length_out).unwrap(),
            p.rdptr(pointer_out).unwrap(),
        )
    }

    #[test]
    fn version_apis_resolve_and_have_generated_x86_stdcall_cleanup() {
        assert!(crate::hle::group_of("version").is_some());
        assert!(lookup("GetFileVersionInfoSizeA").is_some());
        assert!(lookup("GetFileVersionInfoA").is_some());
        assert!(lookup("VerQueryValueA").is_some());
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("version", "GetFileVersionInfoSizeA"),
            Some(8)
        );
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("version", "GetFileVersionInfoA"),
            Some(16)
        );
        assert_eq!(
            crate::x86_abi_generated::cleanup_bytes("version", "VerQueryValueA"),
            Some(16)
        );
    }

    #[test]
    fn version_resource_size_copy_and_root_translation_and_ansi_string_queries_work() {
        let bytes = version_fixture();
        let mut p = process_with_version_resource(&bytes);
        let filename = LOW + 0x100;
        p.write_cstr(filename, "D:\\The Sims 3\\Game\\Bin\\TS3W.exe")
            .unwrap();
        let handle_out = LOW + 0x180;
        p.wr32(handle_out, 0xfeed_beef).unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "GetFileVersionInfoSizeA",
                &[filename as u32, handle_out as u32],
            ),
            Ok(bytes.len() as u64)
        );
        assert_eq!(p.rd32(handle_out).unwrap(), 0);

        let block = load_version_block(&mut p, &bytes);
        let (result, fixed_len, fixed_ptr) = query(&mut p, block, "\\");
        assert_eq!((result, fixed_len), (1, 52));
        assert_eq!(p.rd32(fixed_ptr).unwrap(), VS_FIXEDFILEINFO_SIGNATURE);

        let (result, translation_len, translation_ptr) =
            query(&mut p, block, "\\VarFileInfo\\Translation");
        assert_eq!((result, translation_len), (1, 4));
        assert_eq!(p.rd16(translation_ptr).unwrap(), 0x0409);
        assert_eq!(p.rd16(translation_ptr + 2).unwrap(), 0x04b0);

        let path = "\\StringFileInfo\\040904B0\\FileDescription";
        let (result, string_len, string_ptr) = query(&mut p, block, path);
        assert_eq!((result, string_len), (1, 7)); // character count includes NUL
        assert_eq!(p.read_cstr(string_ptr).unwrap(), "Sims 3");
        let (again, again_len, again_ptr) = query(&mut p, block, path);
        assert_eq!((again, again_len, again_ptr), (1, 7, string_ptr));
        assert_eq!(p.extra.version.ansi_strings.len(), 1);
        assert!(p
            .extra
            .version
            .ansi_strings
            .values()
            .next()
            .unwrap()
            .bytes
            .ends_with(&[0]));
    }

    #[test]
    fn version_queries_fail_for_unknown_or_malformed_resources_without_fabricating_data() {
        let bytes = version_fixture();
        let mut p = process_with_version_resource(&bytes);
        let filename = LOW + 0x100;
        let handle_out = LOW + 0x180;
        p.write_cstr(filename, "absent.exe").unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "GetFileVersionInfoSizeA",
                &[filename as u32, handle_out as u32],
            ),
            Ok(0)
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_FILE_NOT_FOUND);
        assert_eq!(p.rd32(handle_out).unwrap(), 0);

        let mut malformed = bytes;
        malformed[28..32].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let mut p = process_with_version_resource(&malformed);
        p.write_cstr(filename, "TS3W.exe").unwrap();
        assert_eq!(
            call_x86(&mut p, "GetFileVersionInfoSizeA", &[filename as u32, 0],),
            Ok(0)
        );
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_BAD_FORMAT);
    }

    #[test]
    fn truncated_copy_and_missing_query_paths_are_reported_as_failures() {
        let bytes = version_fixture();
        let mut p = process_with_version_resource(&bytes);
        let filename = LOW + 0x100;
        let block = LOW + 0x400;
        p.write_cstr(filename, "TS3W.exe").unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "GetFileVersionInfoA",
                &[filename as u32, 0, 8, block as u32],
            ),
            Ok(1)
        );
        let path = write_query_path(&mut p, "\\");
        let pointer_out = LOW + 0x280;
        let length_out = LOW + 0x288;
        p.wr32(pointer_out, 0xdead_beef).unwrap();
        p.wr32(length_out, 0xdead_beef).unwrap();
        assert_eq!(
            call_x86(
                &mut p,
                "VerQueryValueA",
                &[block as u32, path, pointer_out as u32, length_out as u32],
            ),
            Ok(0)
        );
        assert_eq!(p.rd32(pointer_out).unwrap(), 0);
        assert_eq!(p.rd32(length_out).unwrap(), 0);

        let block = load_version_block(&mut p, &bytes);
        assert_eq!(
            query(&mut p, block, "\\StringFileInfo\\040904B0\\Missing").0,
            0
        );
        assert_eq!(
            query(&mut p, block, "\\StringFileInfo\\malformed\\Name").0,
            0
        );
    }
}
