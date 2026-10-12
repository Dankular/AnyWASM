//! Validated readers for Microsoft C++ exception metadata.

use crate::process::{GuestUnwind, Module, Process, Stop, TEB_ADDR};
use anywasm_cpu::cpu::{CpuMode, RSP};
use anywasm_cpu::mem::PROT_X;

const FUNC_INFO3_BYTES: u64 = 40;
const TRY_BLOCK3_BYTES: u64 = 20;
const HANDLER_TYPE3_BYTES: u64 = 20;
const UNWIND_ENTRY3_BYTES: u64 = 8;
const IP_TO_STATE3_BYTES: u64 = 8;
const THROW_INFO_BYTES: u64 = 16;
const CATCHABLE_TYPE_ARRAY_HEADER_BYTES: u64 = 4;
const CATCHABLE_TYPE3_BYTES: u64 = 28;
const TYPE_DESCRIPTOR_HEADER_BYTES: u64 = 16;
const MAX_EH_ENTRIES: u32 = 4096;
const MAX_TYPE_NAME_BYTES: u64 = 1024;
const EXCEPTION_CONTINUE_SEARCH: u64 = 1;
const EXCEPTION_NONCONTINUABLE: u32 = 0x1;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const DISPATCHER_CONTROL_PC: u64 = 0x00;
const DISPATCHER_IMAGE_BASE: u64 = 0x08;
const DISPATCHER_FUNCTION_ENTRY: u64 = 0x10;
const DISPATCHER_TARGET_IP: u64 = 0x20;
const DISPATCHER_HANDLER_DATA: u64 = 0x38;
const DISPATCHER_HISTORY_TABLE: u64 = 0x40;
const DISPATCHER_SCOPE_INDEX: u64 = 0x48;
const EXCEPTION_RECORD_PARAMETERS: u64 = 24;
const EXCEPTION_RECORD_ARGUMENTS: u64 = 32;
const MSVC_CPP_EXCEPTION: u32 = 0xe06d_7363;
const MSVC_CPP_MAGIC: u64 = 0x1993_0520;
const MAX_CATCH_OBJECT_BYTES: u32 = 16 * 1024 * 1024;

fn malformed(message: impl Into<String>) -> Stop {
    Stop::Crash(format!("MSVC C++ EH metadata: {}", message.into()))
}

fn module_for_image(p: &Process, image_base: u64) -> Result<&Module, Stop> {
    let module = p
        .modules
        .iter()
        .find(|module| module.hle_group.is_none() && module.base == image_base)
        .ok_or_else(|| {
            malformed(format!(
                "image base {image_base:#x} is not a loaded PE image"
            ))
        })?;
    if module.size == 0 || module.base.checked_add(module.size).is_none() {
        return Err(malformed(format!(
            "image {image_base:#x} has an invalid mapped size"
        )));
    }
    Ok(module)
}

fn image_range(module: &Module) -> Result<(u64, u64), Stop> {
    let end = module
        .base
        .checked_add(module.size)
        .ok_or_else(|| malformed("image range overflows the address space"))?;
    Ok((module.base, end))
}

fn check_image_bytes(module: &Module, address: u64, bytes: u64, what: &str) -> Result<(), Stop> {
    let (start, end) = image_range(module)?;
    let read_end = address
        .checked_add(bytes)
        .ok_or_else(|| malformed(format!("{what} address range overflows")))?;
    if address < start || read_end > end || bytes == 0 {
        return Err(malformed(format!(
            "{what} {address:#x}..{read_end:#x} is outside image {start:#x}..{end:#x}"
        )));
    }
    Ok(())
}

fn checked_table_bytes(count: u32, stride: u64, what: &str) -> Result<u64, Stop> {
    if count > MAX_EH_ENTRIES {
        return Err(malformed(format!(
            "{what} count {count} exceeds limit {MAX_EH_ENTRIES}"
        )));
    }
    (count as u64)
        .checked_mul(stride)
        .ok_or_else(|| malformed(format!("{what} byte length overflows")))
}

fn relative_target(
    module: &Module,
    displacement: i32,
    bytes: u64,
    what: &str,
) -> Result<Option<u64>, Stop> {
    if displacement == 0 {
        return Ok(None);
    }
    let address = module
        .base
        .checked_add_signed(displacement as i64)
        .ok_or_else(|| malformed(format!("{what} relative displacement overflows")))?;
    check_image_bytes(module, address, bytes, what)?;
    Ok(Some(address))
}

fn required_relative_target(
    module: &Module,
    displacement: i32,
    bytes: u64,
    what: &str,
) -> Result<u64, Stop> {
    relative_target(module, displacement, bytes, what)?
        .ok_or_else(|| malformed(format!("{what} has a null relative displacement")))
}

fn read_i32(p: &Process, address: u64) -> Result<i32, Stop> {
    Ok(p.rd32(address)? as i32)
}

fn read_type_descriptor(
    p: &Process,
    module: &Module,
    displacement: i32,
) -> Result<Option<TypeDescriptor>, Stop> {
    let Some(address) = relative_target(
        module,
        displacement,
        TYPE_DESCRIPTOR_HEADER_BYTES,
        "TypeDescriptor",
    )?
    else {
        return Ok(None);
    };
    let (_, image_end) = image_range(module)?;
    let name_address = address + TYPE_DESCRIPTOR_HEADER_BYTES;
    let name_limit = image_end.min(name_address.saturating_add(MAX_TYPE_NAME_BYTES));
    if name_address >= name_limit {
        return Err(malformed("TypeDescriptor name starts outside the image"));
    }

    let mut name = Vec::new();
    let mut cursor = name_address;
    while cursor < name_limit {
        let byte = p.rd8(cursor)?;
        if byte == 0 {
            let decorated_name = String::from_utf8(name)
                .map_err(|_| malformed("TypeDescriptor name is not valid ASCII"))?;
            if decorated_name
                .bytes()
                .any(|byte| !(0x20..=0x7e).contains(&byte))
            {
                return Err(malformed(
                    "TypeDescriptor name contains non-printable bytes",
                ));
            }
            return Ok(Some(TypeDescriptor {
                address,
                decorated_name,
            }));
        }
        if !(0x20..=0x7e).contains(&byte) {
            return Err(malformed(
                "TypeDescriptor name contains non-printable bytes",
            ));
        }
        name.push(byte);
        cursor += 1;
    }
    Err(malformed(format!(
        "TypeDescriptor name at {name_address:#x} is not terminated within {MAX_TYPE_NAME_BYTES} bytes"
    )))
}

fn read_handler_type3(p: &Process, module: &Module, address: u64) -> Result<HandlerType3, Stop> {
    check_image_bytes(module, address, HANDLER_TYPE3_BYTES, "HandlerType")?;
    let adjectives = p.rd32(address)?;
    let type_displacement = read_i32(p, address + 4)?;
    let catch_object_displacement = read_i32(p, address + 8)?;
    let handler_displacement = read_i32(p, address + 12)?;
    let frame_displacement = read_i32(p, address + 16)?;
    let handler = required_relative_target(module, handler_displacement, 1, "catch handler")?;
    Ok(HandlerType3 {
        adjectives,
        type_descriptor: read_type_descriptor(p, module, type_displacement)?,
        catch_object_displacement,
        handler,
        frame_displacement,
    })
}

fn read_exception_specification(
    p: &Process,
    module: &Module,
    displacement: i32,
) -> Result<Option<u64>, Stop> {
    let Some(address) = relative_target(module, displacement, 8, "exception specification")? else {
        return Ok(None);
    };
    let count = read_i32(p, address)?;
    if count < 0 || count as u32 > MAX_EH_ENTRIES {
        return Err(malformed(format!(
            "exception specification count {count} is invalid"
        )));
    }
    let array_displacement = read_i32(p, address + 4)?;
    if count != 0 {
        let bytes =
            checked_table_bytes(count as u32, HANDLER_TYPE3_BYTES, "exception specification")?;
        let array = required_relative_target(
            module,
            array_displacement,
            bytes,
            "exception specification type array",
        )?;
        for index in 0..count as u64 {
            read_handler_type3(p, module, array + index * HANDLER_TYPE3_BYTES)?;
        }
    }
    Ok(Some(address))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FuncInfo3 {
    pub magic: u32,
    pub max_state: i32,
    pub unwind_map: Vec<UnwindMapEntry3>,
    pub try_blocks: Vec<TryBlockMapEntry3>,
    pub ip_to_state: Vec<IptoStateMapEntry3>,
    pub unwind_help: Option<u64>,
    pub exception_specification: Option<u64>,
    pub eh_flags: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnwindMapEntry3 {
    pub to_state: i32,
    pub action: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TryBlockMapEntry3 {
    pub try_low: i32,
    pub try_high: i32,
    pub catch_high: i32,
    pub handlers: Vec<HandlerType3>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandlerType3 {
    pub adjectives: u32,
    pub type_descriptor: Option<TypeDescriptor>,
    pub catch_object_displacement: i32,
    pub handler: u64,
    pub frame_displacement: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IptoStateMapEntry3 {
    pub ip_rva: u32,
    pub state: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypeDescriptor {
    pub address: u64,
    pub decorated_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThrowInfo {
    pub attributes: u32,
    pub unwind_function: Option<u64>,
    pub forward_compatibility: Option<u64>,
    pub catchable_types: Vec<CatchableType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatchableType {
    pub properties: u32,
    pub type_descriptor: TypeDescriptor,
    pub this_displacement: Pmd,
    pub size_or_offset: u32,
    pub copy_function: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pmd {
    pub member_displacement: i32,
    pub vbtable_displacement: i32,
    pub vbase_displacement: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FuncInfo4 {
    pub is_catch_funclet: bool,
    pub is_separated: bool,
    pub bbt_flags: Option<u32>,
    pub unwind_map: Vec<UnwindMapEntry4>,
    pub try_blocks: Vec<TryBlockMapEntry4>,
    pub ip_to_state: Vec<IptoStateSegment4>,
    pub frame_displacement: Option<u32>,
    pub has_ehs_flag: bool,
    pub no_except: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnwindMapEntry4 {
    pub to_state: i32,
    pub kind: UnwindEntryKind4,
    pub action: Option<u64>,
    pub object_displacement: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnwindEntryKind4 {
    NoUnwind,
    DestructorWithObject,
    DestructorWithPointerToObject,
    FuncletRva,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TryBlockMapEntry4 {
    pub try_low: i32,
    pub try_high: i32,
    pub catch_high: i32,
    pub handlers: Vec<HandlerType4>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandlerType4 {
    pub adjectives: u32,
    pub type_descriptor: Option<TypeDescriptor>,
    pub catch_object_displacement: Option<u32>,
    pub handler: u64,
    pub continuation_addresses: Vec<ContinuationAddress4>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContinuationAddress4 {
    ImageAddress(u64),
    FunctionRelative(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IptoStateSegment4 {
    pub function_start_rva: Option<u32>,
    pub entries: Vec<IptoStateMapEntry4>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IptoStateMapEntry4 {
    pub function_relative_ip: u32,
    pub state: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CppException {
    pub object_address: u64,
    pub throw_info: ThrowInfo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatchHandler {
    pub adjectives: u32,
    pub decorated_type_name: Option<String>,
    pub catch_object_displacement: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatchMatch {
    pub catchable_type_index: Option<usize>,
    pub decorated_type_name: Option<String>,
    pub adjusted_object_address: u64,
    pub catch_object_displacement: i64,
    pub by_reference: bool,
    pub copy_function: Option<u64>,
    pub object_size: u32,
    pub is_catch_all: bool,
}

pub(crate) fn parse_func_info3(
    p: &Process,
    image_base: u64,
    handler_data: u64,
) -> Result<FuncInfo3, Stop> {
    let module = module_for_image(p, image_base)?;
    check_image_bytes(module, handler_data, 4, "EH3 handler data")?;
    let function_info_displacement = read_i32(p, handler_data)?;
    let function_info = required_relative_target(
        module,
        function_info_displacement,
        FUNC_INFO3_BYTES,
        "EH3 FuncInfo",
    )?;

    let raw_magic = p.rd32(function_info)?;
    let magic = raw_magic & 0x1fff_ffff;
    if !matches!(magic, 0x1993_0520..=0x1993_0522) {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: format!("unsupported MSVC EH3 FuncInfo magic {magic:#x}"),
        });
    }
    let max_state = read_i32(p, function_info + 4)?;
    if !(0..=MAX_EH_ENTRIES as i32).contains(&max_state) {
        return Err(malformed(format!("EH3 maxState {max_state} is invalid")));
    }
    let unwind_displacement = read_i32(p, function_info + 8)?;
    let n_try_blocks = p.rd32(function_info + 12)?;
    let try_displacement = read_i32(p, function_info + 16)?;
    let n_ip_entries = p.rd32(function_info + 20)?;
    let ip_displacement = read_i32(p, function_info + 24)?;
    let unwind_help_displacement = read_i32(p, function_info + 28)?;
    let exception_spec_displacement = read_i32(p, function_info + 32)?;
    let eh_flags = read_i32(p, function_info + 36)?;

    let unwind_bytes =
        checked_table_bytes(max_state as u32, UNWIND_ENTRY3_BYTES, "EH3 unwind map")?;
    let unwind_map = if max_state == 0 {
        if unwind_displacement != 0 {
            relative_target(module, unwind_displacement, 1, "EH3 unwind map")?;
        }
        Vec::new()
    } else {
        let address =
            required_relative_target(module, unwind_displacement, unwind_bytes, "EH3 unwind map")?;
        let mut entries = Vec::with_capacity(max_state as usize);
        for index in 0..max_state as u64 {
            let entry = address + index * UNWIND_ENTRY3_BYTES;
            let to_state = read_i32(p, entry)?;
            if to_state < -1 || to_state >= max_state || to_state >= index as i32 {
                return Err(malformed(format!(
                    "EH3 unwind entry {index} has invalid target state {to_state}"
                )));
            }
            let action_displacement = read_i32(p, entry + 4)?;
            let action = relative_target(module, action_displacement, 1, "EH3 unwind funclet")?;
            entries.push(UnwindMapEntry3 { to_state, action });
        }
        entries
    };

    let try_bytes = checked_table_bytes(n_try_blocks, TRY_BLOCK3_BYTES, "EH3 try-block map")?;
    let mut try_blocks = Vec::with_capacity(n_try_blocks as usize);
    if n_try_blocks == 0 {
        if try_displacement != 0 {
            relative_target(module, try_displacement, 1, "EH3 try-block map")?;
        }
    } else {
        let address =
            required_relative_target(module, try_displacement, try_bytes, "EH3 try-block map")?;
        for index in 0..n_try_blocks as u64 {
            let entry = address + index * TRY_BLOCK3_BYTES;
            let try_low = read_i32(p, entry)?;
            let try_high = read_i32(p, entry + 4)?;
            let catch_high = read_i32(p, entry + 8)?;
            let n_catches = p.rd32(entry + 12)?;
            let handler_displacement = read_i32(p, entry + 16)?;
            if try_low < 0 || try_high < try_low || try_high >= max_state {
                return Err(malformed(format!(
                    "EH3 try block {index} has invalid state range {try_low}..={try_high}"
                )));
            }
            if catch_high < -1 || catch_high >= max_state {
                return Err(malformed(format!(
                    "EH3 try block {index} has invalid catchHigh {catch_high}"
                )));
            }
            let handler_bytes =
                checked_table_bytes(n_catches, HANDLER_TYPE3_BYTES, "EH3 handler array")?;
            let mut handlers = Vec::with_capacity(n_catches as usize);
            if n_catches != 0 {
                let handler_array = required_relative_target(
                    module,
                    handler_displacement,
                    handler_bytes,
                    "EH3 handler array",
                )?;
                for handler_index in 0..n_catches as u64 {
                    handlers.push(read_handler_type3(
                        p,
                        module,
                        handler_array + handler_index * HANDLER_TYPE3_BYTES,
                    )?);
                }
            } else if handler_displacement != 0 {
                relative_target(module, handler_displacement, 1, "EH3 handler array")?;
            }
            try_blocks.push(TryBlockMapEntry3 {
                try_low,
                try_high,
                catch_high,
                handlers,
            });
        }
    }

    let ip_bytes = checked_table_bytes(n_ip_entries, IP_TO_STATE3_BYTES, "EH3 IP-to-state map")?;
    let mut ip_to_state = Vec::with_capacity(n_ip_entries as usize);
    if n_ip_entries == 0 {
        if ip_displacement != 0 {
            relative_target(module, ip_displacement, 1, "EH3 IP-to-state map")?;
        }
    } else {
        let address =
            required_relative_target(module, ip_displacement, ip_bytes, "EH3 IP-to-state map")?;
        let (_, image_end) = image_range(module)?;
        let image_size = image_end - image_base;
        let mut previous_ip = None;
        for index in 0..n_ip_entries as u64 {
            let entry = address + index * IP_TO_STATE3_BYTES;
            let ip_rva = p.rd32(entry)?;
            let state = read_i32(p, entry + 4)?;
            if ip_rva as u64 >= image_size || state < -1 || state >= max_state {
                return Err(malformed(format!(
                    "EH3 IP-to-state entry {index} has invalid RVA/state {ip_rva:#x}/{state}"
                )));
            }
            if previous_ip.is_some_and(|previous| ip_rva < previous) {
                return Err(malformed("EH3 IP-to-state map is not sorted by IP RVA"));
            }
            previous_ip = Some(ip_rva);
            ip_to_state.push(IptoStateMapEntry3 { ip_rva, state });
        }
    }

    let unwind_help = relative_target(module, unwind_help_displacement, 1, "EH3 unwind helper")?;
    let exception_specification =
        read_exception_specification(p, module, exception_spec_displacement)?;

    Ok(FuncInfo3 {
        magic,
        max_state,
        unwind_map,
        try_blocks,
        ip_to_state,
        unwind_help,
        exception_specification,
        eh_flags,
    })
}

pub(crate) fn parse_throw_info(
    p: &Process,
    image_base: u64,
    throw_info: u64,
) -> Result<ThrowInfo, Stop> {
    let module = module_for_image(p, image_base)?;
    check_image_bytes(&module, throw_info, THROW_INFO_BYTES, "ThrowInfo")?;
    let attributes = p.rd32(throw_info)?;
    let unwind_displacement = read_i32(p, throw_info + 4)?;
    let forward_compatibility_displacement = read_i32(p, throw_info + 8)?;
    let catchable_array_displacement = read_i32(p, throw_info + 12)?;
    let unwind_function = relative_target(&module, unwind_displacement, 1, "throw unwind funclet")?;
    let forward_compatibility = relative_target(
        &module,
        forward_compatibility_displacement,
        1,
        "throw forward-compatibility funclet",
    )?;
    let array = required_relative_target(
        &module,
        catchable_array_displacement,
        CATCHABLE_TYPE_ARRAY_HEADER_BYTES,
        "CatchableTypeArray",
    )?;
    let count = read_i32(p, array)?;
    if count <= 0 || count as u32 > MAX_EH_ENTRIES {
        return Err(malformed(format!(
            "CatchableTypeArray count {count} is invalid"
        )));
    }
    let bytes = checked_table_bytes(count as u32, 4, "CatchableTypeArray pointer list")?;
    let list = array
        .checked_add(CATCHABLE_TYPE_ARRAY_HEADER_BYTES)
        .ok_or_else(|| malformed("CatchableTypeArray pointer-list address overflows"))?;
    check_image_bytes(&module, list, bytes, "CatchableTypeArray pointer list")?;

    let mut catchable_types = Vec::with_capacity(count as usize);
    for index in 0..count as u64 {
        let pointer_displacement = read_i32(p, list + index * 4)?;
        let address = required_relative_target(
            &module,
            pointer_displacement,
            CATCHABLE_TYPE3_BYTES,
            "CatchableType",
        )?;
        let properties = p.rd32(address)?;
        let type_displacement = read_i32(p, address + 4)?;
        let type_descriptor = read_type_descriptor(p, &module, type_displacement)?
            .ok_or_else(|| malformed("CatchableType has a null TypeDescriptor"))?;
        let this_displacement = Pmd {
            member_displacement: read_i32(p, address + 8)?,
            vbtable_displacement: read_i32(p, address + 12)?,
            vbase_displacement: read_i32(p, address + 16)?,
        };
        let size_or_offset = p.rd32(address + 20)?;
        if size_or_offset == 0 {
            return Err(malformed("CatchableType has a zero object size/offset"));
        }
        let copy_displacement = read_i32(p, address + 24)?;
        let copy_function = relative_target(&module, copy_displacement, 1, "catch copy funclet")?;
        catchable_types.push(CatchableType {
            properties,
            type_descriptor,
            this_displacement,
            size_or_offset,
            copy_function,
        });
    }

    Ok(ThrowInfo {
        attributes,
        unwind_function,
        forward_compatibility,
        catchable_types,
    })
}

pub(crate) fn decode_eh4_uint(p: &Process, cursor: &mut u64, end: u64) -> Result<u32, Stop> {
    let start = *cursor;
    if start >= end {
        return Err(malformed("truncated EH4 compressed integer"));
    }
    let first = p.rd8(start)?;
    let marker = first & 0x0f;
    let length = marker.trailing_ones() as u64 + 1;
    let next = start
        .checked_add(length)
        .ok_or_else(|| malformed("EH4 compressed integer address overflows"))?;
    if next > end {
        return Err(malformed(format!(
            "truncated {length}-byte EH4 compressed integer at {start:#x}"
        )));
    }

    let value = if length == 5 {
        if first != 0x0f {
            return Err(malformed(
                "five-byte EH4 integer has nonzero reserved prefix bits",
            ));
        }
        let mut value = 0u32;
        for index in 0..4u64 {
            value |= (p.rd8(start + 1 + index)? as u32) << (index * 8);
        }
        value
    } else {
        let mut encoded = 0u64;
        for index in 0..length {
            encoded |= (p.rd8(start + index)? as u64) << (index * 8);
        }
        (encoded >> length) as u32
    };

    let minimum = match length {
        1 => 0,
        2 => 1 << 7,
        3 => 1 << 14,
        4 => 1 << 21,
        5 => 1 << 28,
        _ => unreachable!("length is derived from a four-bit EH4 marker"),
    };
    if value < minimum {
        return Err(malformed(format!(
            "overlong {length}-byte EH4 integer encodes {value:#x}"
        )));
    }
    *cursor = next;
    Ok(value)
}

fn decode_eh4_count(p: &Process, cursor: &mut u64, end: u64, what: &str) -> Result<u32, Stop> {
    let count = decode_eh4_uint(p, cursor, end)?;
    if count > MAX_EH_ENTRIES {
        return Err(malformed(format!(
            "{what} count {count} exceeds limit {MAX_EH_ENTRIES}"
        )));
    }
    Ok(count)
}

fn read_eh4_i32(p: &Process, module: &Module, cursor: &mut u64, what: &str) -> Result<i32, Stop> {
    check_image_bytes(module, *cursor, 4, what)?;
    let value = read_i32(p, *cursor)?;
    *cursor = cursor
        .checked_add(4)
        .ok_or_else(|| malformed(format!("{what} cursor overflows")))?;
    Ok(value)
}

fn decode_eh4_state(value: u32, what: &str) -> Result<i32, Stop> {
    i32::try_from(value as i64 - 1)
        .map_err(|_| malformed(format!("{what} state encoding overflows")))
}

fn decode_eh4_direct_state(value: u32) -> i32 {
    value as i32
}

fn parse_unwind_map4(
    p: &Process,
    module: &Module,
    displacement: i32,
) -> Result<Vec<UnwindMapEntry4>, Stop> {
    let Some(address) = relative_target(module, displacement, 1, "EH4 unwind map")? else {
        return Ok(Vec::new());
    };
    let (_, end) = image_range(module)?;
    let mut cursor = address;
    let count = decode_eh4_count(p, &mut cursor, end, "EH4 unwind map")?;
    let entries_start = cursor;
    let mut entries = Vec::with_capacity(count as usize);
    let mut starts = Vec::with_capacity(count as usize);
    let mut target_offsets = Vec::with_capacity(count as usize);

    for index in 0..count {
        let entry_start = cursor;
        let packed = decode_eh4_uint(p, &mut cursor, end)?;
        let next_offset = (packed >> 2) as u64;
        if next_offset == 0 {
            return Err(malformed(format!(
                "EH4 unwind entry {index} has a zero previous-entry offset"
            )));
        }
        let kind = match packed & 3 {
            0 => UnwindEntryKind4::NoUnwind,
            1 => UnwindEntryKind4::DestructorWithObject,
            2 => UnwindEntryKind4::DestructorWithPointerToObject,
            3 => UnwindEntryKind4::FuncletRva,
            _ => unreachable!(),
        };
        let (action, object_displacement) = match kind {
            UnwindEntryKind4::NoUnwind => (None, None),
            UnwindEntryKind4::DestructorWithObject
            | UnwindEntryKind4::DestructorWithPointerToObject => {
                let action_displacement =
                    read_eh4_i32(p, module, &mut cursor, "EH4 unwind action")?;
                let action =
                    required_relative_target(module, action_displacement, 1, "EH4 unwind action")?;
                let object_displacement = decode_eh4_uint(p, &mut cursor, end)?;
                (Some(action), Some(object_displacement))
            }
            UnwindEntryKind4::FuncletRva => {
                let action_displacement =
                    read_eh4_i32(p, module, &mut cursor, "EH4 unwind funclet")?;
                let action =
                    required_relative_target(module, action_displacement, 1, "EH4 unwind funclet")?;
                (Some(action), None)
            }
        };
        starts.push(entry_start);
        target_offsets.push(
            entry_start.checked_sub(next_offset).ok_or_else(|| {
                malformed(format!("EH4 unwind entry {index} points before memory"))
            })?,
        );
        entries.push(UnwindMapEntry4 {
            to_state: -2,
            kind,
            action,
            object_displacement,
        });
    }

    for index in 0..count as usize {
        let target = target_offsets[index];
        let target_state = if target == entries_start - 1 {
            -1
        } else if let Some(previous) = starts[..index].iter().position(|start| *start == target) {
            previous as i32
        } else {
            return Err(malformed(format!(
                "EH4 unwind entry {index} points to a non-previous entry at {target:#x}"
            )));
        };
        entries[index].to_state = target_state;
    }
    Ok(entries)
}

fn parse_handler_map4(
    p: &Process,
    module: &Module,
    displacement: i32,
) -> Result<Vec<HandlerType4>, Stop> {
    let Some(address) = relative_target(module, displacement, 1, "EH4 handler map")? else {
        return Ok(Vec::new());
    };
    let (_, end) = image_range(module)?;
    let mut cursor = address;
    let count = decode_eh4_count(p, &mut cursor, end, "EH4 handler map")?;
    let mut handlers = Vec::with_capacity(count as usize);
    for index in 0..count {
        check_image_bytes(module, cursor, 1, "EH4 HandlerType header")?;
        let header = p.rd8(cursor)?;
        cursor += 1;
        let continuation_count = (header >> 4) & 3;
        if header & 0xc0 != 0 || continuation_count == 3 {
            return Err(malformed(format!(
                "EH4 handler {index} has reserved header bits {header:#04x}"
            )));
        }
        let adjectives = if header & 1 != 0 {
            decode_eh4_uint(p, &mut cursor, end)?
        } else {
            0
        };
        let type_descriptor = if header & 2 != 0 {
            let type_displacement = read_eh4_i32(p, module, &mut cursor, "EH4 catch type")?;
            Some(
                read_type_descriptor(p, module, type_displacement)?
                    .ok_or_else(|| malformed("EH4 typed catch has a null TypeDescriptor"))?,
            )
        } else {
            None
        };
        let catch_object_displacement = if header & 4 != 0 {
            Some(decode_eh4_uint(p, &mut cursor, end)?)
        } else {
            None
        };
        let handler_displacement = read_eh4_i32(p, module, &mut cursor, "EH4 catch handler")?;
        let handler =
            required_relative_target(module, handler_displacement, 1, "EH4 catch handler")?;
        let mut continuation_addresses = Vec::with_capacity(continuation_count as usize);
        for _ in 0..continuation_count {
            if header & 8 != 0 {
                let displacement =
                    read_eh4_i32(p, module, &mut cursor, "EH4 continuation address")?;
                let address =
                    required_relative_target(module, displacement, 1, "EH4 continuation address")?;
                continuation_addresses.push(ContinuationAddress4::ImageAddress(address));
            } else {
                let offset = decode_eh4_uint(p, &mut cursor, end)?;
                if offset as u64 >= module.size {
                    return Err(malformed(format!(
                        "EH4 function-relative continuation {offset:#x} exceeds image size"
                    )));
                }
                continuation_addresses.push(ContinuationAddress4::FunctionRelative(offset));
            }
        }
        handlers.push(HandlerType4 {
            adjectives,
            type_descriptor,
            catch_object_displacement,
            handler,
            continuation_addresses,
        });
    }
    Ok(handlers)
}

fn parse_try_block_map4(
    p: &Process,
    module: &Module,
    displacement: i32,
    max_state: usize,
) -> Result<Vec<TryBlockMapEntry4>, Stop> {
    let Some(address) = relative_target(module, displacement, 1, "EH4 try-block map")? else {
        return Ok(Vec::new());
    };
    let (_, end) = image_range(module)?;
    let mut cursor = address;
    let count = decode_eh4_count(p, &mut cursor, end, "EH4 try-block map")?;
    let mut blocks = Vec::with_capacity(count as usize);
    for index in 0..count {
        // TryBlockMapEntry4 stores these as signed values directly. The state + 1
        // representation is specific to IP-to-state maps, where zero means -1.
        let try_low = decode_eh4_direct_state(decode_eh4_uint(p, &mut cursor, end)?);
        let try_high = decode_eh4_direct_state(decode_eh4_uint(p, &mut cursor, end)?);
        let catch_high = decode_eh4_direct_state(decode_eh4_uint(p, &mut cursor, end)?);
        let handler_displacement = read_eh4_i32(p, module, &mut cursor, "EH4 handler map RVA")?;
        if try_low < -1 || try_high < try_low || try_high as usize >= max_state && try_high != -1 {
            return Err(malformed(format!(
                "EH4 try block {index} has invalid state range {try_low}..={try_high}"
            )));
        }
        if catch_high < try_high || (catch_high >= 0 && catch_high as usize >= max_state) {
            return Err(malformed(format!(
                "EH4 try block {index} has invalid catchHigh {catch_high}"
            )));
        }
        let handlers = parse_handler_map4(p, module, handler_displacement)?;
        blocks.push(TryBlockMapEntry4 {
            try_low,
            try_high,
            catch_high,
            handlers,
        });
    }
    Ok(blocks)
}

fn parse_ip_to_state_map4(
    p: &Process,
    module: &Module,
    displacement: i32,
    function_start_rva: Option<u32>,
    max_state: usize,
) -> Result<Vec<IptoStateMapEntry4>, Stop> {
    let Some(address) = relative_target(module, displacement, 1, "EH4 IP-to-state map")? else {
        return Ok(Vec::new());
    };
    let (_, end) = image_range(module)?;
    let mut cursor = address;
    let count = decode_eh4_count(p, &mut cursor, end, "EH4 IP-to-state map")?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut previous_ip = 0u32;
    for index in 0..count {
        let delta = decode_eh4_uint(p, &mut cursor, end)?;
        let ip = previous_ip
            .checked_add(delta)
            .ok_or_else(|| malformed(format!("EH4 IP-to-state entry {index} RVA overflows")))?;
        let state = decode_eh4_state(decode_eh4_uint(p, &mut cursor, end)?, "EH4 IP state")?;
        if state < -1 || (state >= 0 && state as usize >= max_state) {
            return Err(malformed(format!(
                "EH4 IP-to-state entry {index} has invalid state {state}"
            )));
        }
        let absolute_rva = function_start_rva
            .map(|start| start.checked_add(ip))
            .unwrap_or(Some(ip))
            .ok_or_else(|| malformed(format!("EH4 IP-to-state entry {index} RVA overflows")))?;
        if absolute_rva as u64 >= module.size {
            return Err(malformed(format!(
                "EH4 IP-to-state entry {index} RVA {absolute_rva:#x} is outside the image"
            )));
        }
        entries.push(IptoStateMapEntry4 {
            function_relative_ip: ip,
            state,
        });
        previous_ip = ip;
    }
    Ok(entries)
}

fn parse_separated_ip_map4(
    p: &Process,
    module: &Module,
    displacement: i32,
    max_state: usize,
) -> Result<Vec<IptoStateSegment4>, Stop> {
    let address = required_relative_target(module, displacement, 1, "EH4 separated IP map")?;
    let (_, end) = image_range(module)?;
    let mut cursor = address;
    let count = decode_eh4_count(p, &mut cursor, end, "EH4 separated IP map")?;
    let mut segments = Vec::with_capacity(count as usize);
    let mut previous_start = None;
    for index in 0..count {
        check_image_bytes(module, cursor, 4, "EH4 separated function start")?;
        let start_rva = p.rd32(cursor)?;
        cursor += 4;
        let map_displacement = read_eh4_i32(p, module, &mut cursor, "EH4 separated IP map RVA")?;
        if start_rva as u64 >= module.size || previous_start.is_some_and(|value| start_rva <= value)
        {
            return Err(malformed(format!(
                "EH4 separated IP map entry {index} has invalid or unsorted start RVA {start_rva:#x}"
            )));
        }
        let entries =
            parse_ip_to_state_map4(p, module, map_displacement, Some(start_rva), max_state)?;
        previous_start = Some(start_rva);
        segments.push(IptoStateSegment4 {
            function_start_rva: Some(start_rva),
            entries,
        });
    }
    Ok(segments)
}

pub(crate) fn parse_func_info4(
    p: &Process,
    image_base: u64,
    handler_data: u64,
) -> Result<FuncInfo4, Stop> {
    let module = module_for_image(p, image_base)?;
    check_image_bytes(module, handler_data, 4, "EH4 handler data")?;
    let func_info_displacement = read_i32(p, handler_data)?;
    let func_info = required_relative_target(module, func_info_displacement, 1, "EH4 FuncInfo")?;
    let (_, end) = image_range(module)?;
    let mut cursor = func_info;
    let header = p.rd8(cursor)?;
    cursor += 1;
    if header & 0x80 != 0 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: format!("reserved MSVC EH4 FuncInfo header bit is set ({header:#04x})"),
        });
    }
    let is_catch_funclet = header & 1 != 0;
    let is_separated = header & 2 != 0;
    let bbt_flags = if header & 4 != 0 {
        Some(decode_eh4_uint(p, &mut cursor, end)?)
    } else {
        None
    };
    let unwind_displacement = if header & 8 != 0 {
        read_eh4_i32(p, module, &mut cursor, "EH4 unwind map RVA")?
    } else {
        0
    };
    let try_displacement = if header & 0x10 != 0 {
        read_eh4_i32(p, module, &mut cursor, "EH4 try-block map RVA")?
    } else {
        0
    };
    let ip_displacement = read_eh4_i32(p, module, &mut cursor, "EH4 IP-to-state map RVA")?;
    let frame_displacement = if is_catch_funclet {
        Some(decode_eh4_uint(p, &mut cursor, end)?)
    } else {
        None
    };
    if cursor > end {
        return Err(malformed("EH4 FuncInfo record extends past its image"));
    }
    if header & 8 != 0 && unwind_displacement == 0 {
        return Err(malformed("EH4 FuncInfo marks a null unwind map as present"));
    }
    if header & 0x10 != 0 && try_displacement == 0 {
        return Err(malformed(
            "EH4 FuncInfo marks a null try-block map as present",
        ));
    }

    let unwind_map = parse_unwind_map4(p, module, unwind_displacement)?;
    let try_blocks = parse_try_block_map4(p, module, try_displacement, unwind_map.len())?;
    let ip_to_state = if is_separated {
        parse_separated_ip_map4(p, module, ip_displacement, unwind_map.len())?
    } else {
        vec![IptoStateSegment4 {
            function_start_rva: None,
            entries: parse_ip_to_state_map4(p, module, ip_displacement, None, unwind_map.len())?,
        }]
    };

    Ok(FuncInfo4 {
        is_catch_funclet,
        is_separated,
        bbt_flags,
        unwind_map,
        try_blocks,
        ip_to_state,
        frame_displacement,
        has_ehs_flag: header & 0x20 != 0,
        no_except: header & 0x40 != 0,
    })
}

const TI_IS_CONST: u32 = 0x01;
const TI_IS_VOLATILE: u32 = 0x02;
const TI_IS_UNALIGNED: u32 = 0x04;
const TI_IS_PURE: u32 = 0x08;
const TI_IS_WINRT: u32 = 0x10;
const CT_BY_REFERENCE_ONLY: u32 = 0x02;
const CT_HAS_VIRTUAL_BASE: u32 = 0x04;
const HT_IS_CONST: u32 = 0x01;
const HT_IS_VOLATILE: u32 = 0x02;
const HT_IS_UNALIGNED: u32 = 0x04;
const HT_IS_REFERENCE: u32 = 0x08;
const HT_IS_RESUMABLE: u32 = 0x10;
const HT_IS_STD_DOTDOT: u32 = 0x40;
const HT_IS_BAD_ALLOC_COMPAT: u32 = 0x80;
const HT_IS_COMPLUS_EH: u32 = 0x8000_0000;
const HT_KNOWN_ADJECTIVES: u32 = HT_IS_CONST
    | HT_IS_VOLATILE
    | HT_IS_UNALIGNED
    | HT_IS_REFERENCE
    | HT_IS_RESUMABLE
    | HT_IS_STD_DOTDOT
    | HT_IS_BAD_ALLOC_COMPAT
    | HT_IS_COMPLUS_EH;

fn handler_from_eh3(handler: &HandlerType3) -> CatchHandler {
    CatchHandler {
        adjectives: handler.adjectives,
        decorated_type_name: handler
            .type_descriptor
            .as_ref()
            .map(|descriptor| descriptor.decorated_name.clone()),
        catch_object_displacement: handler.catch_object_displacement as i64,
    }
}

fn handler_from_eh4(handler: &HandlerType4) -> CatchHandler {
    CatchHandler {
        adjectives: handler.adjectives,
        decorated_type_name: handler
            .type_descriptor
            .as_ref()
            .map(|descriptor| descriptor.decorated_name.clone()),
        catch_object_displacement: handler.catch_object_displacement.unwrap_or_default() as i32
            as i64,
    }
}

pub(crate) fn match_handler3(
    p: &Process,
    exception: &CppException,
    handler: &HandlerType3,
) -> Result<Option<CatchMatch>, Stop> {
    match_handler(p, exception, &handler_from_eh3(handler))
}

pub(crate) fn match_handler4(
    p: &Process,
    exception: &CppException,
    handler: &HandlerType4,
) -> Result<Option<CatchMatch>, Stop> {
    match_handler(p, exception, &handler_from_eh4(handler))
}

pub(crate) fn match_handler(
    p: &Process,
    exception: &CppException,
    handler: &CatchHandler,
) -> Result<Option<CatchMatch>, Stop> {
    if exception.object_address == 0 || p.mem.prot_of(exception.object_address).is_none() {
        return Err(malformed(format!(
            "thrown object address {:#x} is not mapped",
            exception.object_address
        )));
    }
    let adjectives = handler.adjectives;
    if adjectives & !HT_KNOWN_ADJECTIVES != 0 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: format!("unsupported MSVC catch adjectives {adjectives:#x}"),
        });
    }
    if adjectives & (HT_IS_RESUMABLE | HT_IS_BAD_ALLOC_COMPAT | HT_IS_COMPLUS_EH) != 0 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: format!("unsupported MSVC catch features {:#x}", adjectives),
        });
    }

    let is_catch_all = handler
        .decorated_type_name
        .as_deref()
        .is_none_or(str::is_empty);
    if exception.throw_info.attributes & TI_IS_PURE != 0 {
        return Ok(None);
    }
    if exception.throw_info.attributes & TI_IS_WINRT != 0 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: "WinRT C++ exception type matching is not implemented".into(),
        });
    }
    if is_catch_all {
        return Ok(Some(CatchMatch {
            catchable_type_index: None,
            decorated_type_name: None,
            adjusted_object_address: exception.object_address,
            catch_object_displacement: handler.catch_object_displacement,
            by_reference: false,
            copy_function: None,
            object_size: 0,
            is_catch_all: true,
        }));
    }

    let catch_name = handler.decorated_type_name.as_deref().unwrap();
    let by_reference = adjectives & HT_IS_REFERENCE != 0;
    for (index, catchable) in exception.throw_info.catchable_types.iter().enumerate() {
        if catchable.type_descriptor.decorated_name != catch_name {
            continue;
        }
        if catchable.properties & CT_BY_REFERENCE_ONLY != 0 && !by_reference {
            continue;
        }
        let thrown_attributes = exception.throw_info.attributes;
        if by_reference
            && ((thrown_attributes & TI_IS_CONST != 0 && adjectives & HT_IS_CONST == 0)
                || (thrown_attributes & TI_IS_VOLATILE != 0 && adjectives & HT_IS_VOLATILE == 0)
                || (thrown_attributes & TI_IS_UNALIGNED != 0 && adjectives & HT_IS_UNALIGNED == 0))
        {
            continue;
        }
        if catchable.properties & CT_HAS_VIRTUAL_BASE != 0
            || catchable.this_displacement.vbtable_displacement != -1
        {
            return Err(Stop::Unimplemented {
                dll: "vcruntime".into(),
                name: format!("virtual-base catch conversion for {catch_name}"),
            });
        }
        let adjusted_object_address = exception
            .object_address
            .checked_add_signed(catchable.this_displacement.member_displacement as i64)
            .ok_or_else(|| malformed("catchable base-subobject address overflows"))?;
        if p.mem.prot_of(adjusted_object_address).is_none() {
            return Err(malformed(format!(
                "catchable base-subobject address {adjusted_object_address:#x} is not mapped"
            )));
        }
        return Ok(Some(CatchMatch {
            catchable_type_index: Some(index),
            decorated_type_name: Some(catch_name.to_string()),
            adjusted_object_address,
            catch_object_displacement: handler.catch_object_displacement,
            by_reference,
            copy_function: if by_reference {
                None
            } else {
                catchable.copy_function
            },
            object_size: catchable.size_or_offset,
            is_catch_all: false,
        }));
    }
    Ok(None)
}

#[derive(Clone, Copy)]
struct HandlerCall {
    exception_record: u64,
    establisher: u64,
    context_record: u64,
    dispatcher_context: u64,
    exception_flags: u32,
    control_pc: u64,
    image_base: u64,
    function_entry: u64,
    target_ip: u64,
    handler_data: u64,
    history_table: u64,
}

fn handler_call(p: &Process) -> Result<HandlerCall, Stop> {
    if p.cpu.mode != CpuMode::X86_64 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: "MSVC C++ frame handlers for PE32".into(),
        });
    }
    let (exception_record, establisher, context_record, dispatcher_context) =
        (p.arg(0), p.arg(1), p.arg(2), p.arg(3));
    if exception_record == 0 || establisher == 0 || context_record == 0 || dispatcher_context == 0 {
        return Err(malformed("frame handler received a null dispatcher record"));
    }
    Ok(HandlerCall {
        exception_record,
        establisher,
        context_record,
        dispatcher_context,
        exception_flags: p.rd32(exception_record + 4)?,
        control_pc: p.rd64(dispatcher_context + DISPATCHER_CONTROL_PC)?,
        image_base: p.rd64(dispatcher_context + DISPATCHER_IMAGE_BASE)?,
        function_entry: p.rd64(dispatcher_context + DISPATCHER_FUNCTION_ENTRY)?,
        target_ip: p.rd64(dispatcher_context + DISPATCHER_TARGET_IP)?,
        handler_data: p.rd64(dispatcher_context + DISPATCHER_HANDLER_DATA)?,
        history_table: p.rd64(dispatcher_context + DISPATCHER_HISTORY_TABLE)?,
    })
}

fn cpp_exception_from_record(
    p: &Process,
    exception_record: u64,
) -> Result<Option<CppException>, Stop> {
    if p.rd32(exception_record)? != MSVC_CPP_EXCEPTION {
        return Ok(None);
    }
    let parameter_count = p.rd32(exception_record + EXCEPTION_RECORD_PARAMETERS)?;
    if parameter_count != 4 {
        return Err(malformed(format!(
            "MSVC C++ exception record has {parameter_count} parameters, expected 4"
        )));
    }
    let magic = p.rd64(exception_record + EXCEPTION_RECORD_ARGUMENTS)?;
    if magic != MSVC_CPP_MAGIC {
        return Err(malformed(format!(
            "MSVC C++ exception magic is {magic:#x}, expected {MSVC_CPP_MAGIC:#x}"
        )));
    }
    let object_address = p.rd64(exception_record + EXCEPTION_RECORD_ARGUMENTS + 8)?;
    let throw_info_address = p.rd64(exception_record + EXCEPTION_RECORD_ARGUMENTS + 16)?;
    let image_base = p.rd64(exception_record + EXCEPTION_RECORD_ARGUMENTS + 24)?;
    let throw_info = parse_throw_info(p, image_base, throw_info_address)?;
    if object_address == 0 || p.mem.prot_of(object_address).is_none() {
        return Err(malformed(format!(
            "thrown object address {object_address:#x} is not mapped"
        )));
    }
    Ok(Some(CppException {
        object_address,
        throw_info,
    }))
}

fn current_state3(info: &FuncInfo3, image_base: u64, control_pc: u64) -> Result<i32, Stop> {
    let control_rva = control_pc
        .checked_sub(image_base)
        .ok_or_else(|| malformed("control PC precedes the EH3 image base"))?;
    Ok(info
        .ip_to_state
        .iter()
        .take_while(|entry| entry.ip_rva as u64 <= control_rva)
        .last()
        .map_or(-1, |entry| entry.state))
}

fn function_start_rva(p: &Process, image_base: u64, function_entry: u64) -> Result<u32, Stop> {
    let module = module_for_image(p, image_base)?;
    check_image_bytes(module, function_entry, 12, "runtime function entry")?;
    let start_rva = p.rd32(function_entry)?;
    if start_rva as u64 >= module.size {
        return Err(malformed(format!(
            "runtime function start RVA {start_rva:#x} is outside the image"
        )));
    }
    Ok(start_rva)
}

fn current_state4(
    p: &Process,
    info: &FuncInfo4,
    image_base: u64,
    function_entry: u64,
    control_pc: u64,
) -> Result<i32, Stop> {
    let function_start = function_start_rva(p, image_base, function_entry)?;
    let control_rva = control_pc
        .checked_sub(image_base)
        .ok_or_else(|| malformed("control PC precedes the EH4 image base"))?;
    let segment = if info.is_separated {
        info.ip_to_state
            .iter()
            .filter_map(|segment| {
                segment
                    .function_start_rva
                    .filter(|start| *start as u64 <= control_rva)
                    .map(|start| (start, segment))
            })
            .max_by_key(|(start, _)| *start)
            .map(|(_, segment)| segment)
            .ok_or_else(|| malformed("EH4 separated IP map has no segment for control PC"))?
    } else {
        info.ip_to_state
            .first()
            .ok_or_else(|| malformed("EH4 IP-to-state map has no segment"))?
    };
    let segment_start = segment.function_start_rva.unwrap_or(function_start) as u64;
    let function_relative_pc = control_rva
        .checked_sub(segment_start)
        .ok_or_else(|| malformed("control PC precedes its EH4 IP-map segment"))?;
    Ok(segment
        .entries
        .iter()
        .take_while(|entry| entry.function_relative_ip as u64 <= function_relative_pc)
        .last()
        .map_or(-1, |entry| entry.state))
}

fn active_try_indices<T>(
    try_blocks: &[T],
    state: i32,
    range: impl Fn(&T) -> (i32, i32),
) -> Vec<usize> {
    let mut indices = try_blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let (low, high) = range(block);
            (state >= low && state <= high).then_some(index)
        })
        .collect::<Vec<_>>();
    indices.sort_by_key(|&index| {
        let (low, high) = range(&try_blocks[index]);
        (std::cmp::Reverse(low), high)
    });
    indices
}

fn select_catch3(
    p: &Process,
    exception: &CppException,
    info: &FuncInfo3,
    state: i32,
    target_ip: Option<u64>,
) -> Result<Option<(usize, usize, CatchMatch)>, Stop> {
    for try_index in active_try_indices(&info.try_blocks, state, |block| {
        (block.try_low, block.try_high)
    }) {
        for (handler_index, handler) in info.try_blocks[try_index].handlers.iter().enumerate() {
            if target_ip.is_some_and(|ip| ip != handler.handler) {
                continue;
            }
            if let Some(matched) = match_handler3(p, exception, handler)? {
                return Ok(Some((try_index, handler_index, matched)));
            }
        }
    }
    Ok(None)
}

fn select_catch4(
    p: &Process,
    exception: &CppException,
    info: &FuncInfo4,
    state: i32,
    target_ip: Option<u64>,
) -> Result<Option<(usize, usize, CatchMatch)>, Stop> {
    for try_index in active_try_indices(&info.try_blocks, state, |block| {
        (block.try_low, block.try_high)
    }) {
        for (handler_index, handler) in info.try_blocks[try_index].handlers.iter().enumerate() {
            if target_ip.is_some_and(|ip| ip != handler.handler) {
                continue;
            }
            if let Some(matched) = match_handler4(p, exception, handler)? {
                return Ok(Some((try_index, handler_index, matched)));
            }
        }
    }
    Ok(None)
}

fn stack_bounds(p: &Process) -> Result<(u64, u64), Stop> {
    let stack_base = p.rd64(TEB_ADDR + 0x08)?;
    let stack_limit = p.rd64(TEB_ADDR + 0x10)?;
    if stack_limit >= stack_base {
        return Err(malformed("TEB stack bounds are invalid"));
    }
    Ok((stack_limit, stack_base))
}

fn frame_address(frame: u64, displacement: i64, what: &str) -> Result<u64, Stop> {
    frame
        .checked_add_signed(displacement)
        .ok_or_else(|| malformed(format!("{what} address overflows")))
}

pub(crate) fn build_catch_object(
    p: &mut Process,
    frame: u64,
    exception: &CppException,
    matched: &CatchMatch,
) -> Result<u64, Stop> {
    if matched.is_catch_all {
        return Ok(0);
    }
    let source_size = matched.object_size;
    let destination_size = if matched.by_reference { 8 } else { source_size };
    if source_size == 0
        || source_size > MAX_CATCH_OBJECT_BYTES
        || destination_size > MAX_CATCH_OBJECT_BYTES
    {
        return Err(malformed(format!(
            "catch object size {source_size} is outside the supported range"
        )));
    }
    let destination = frame_address(frame, matched.catch_object_displacement, "catch object")?;
    let (stack_limit, stack_base) = stack_bounds(p)?;
    let destination_end = destination
        .checked_add(destination_size as u64)
        .ok_or_else(|| malformed("catch object range overflows"))?;
    if destination < stack_limit || destination_end > stack_base {
        return Err(malformed(format!(
            "catch object range {destination:#x}..{destination_end:#x} is outside the TEB stack"
        )));
    }
    let source_end = matched
        .adjusted_object_address
        .checked_add(source_size as u64)
        .ok_or_else(|| malformed("thrown object range overflows"))?;
    let thrown_size = exception
        .throw_info
        .catchable_types
        .first()
        .map(|catchable| catchable.size_or_offset)
        .ok_or_else(|| malformed("ThrowInfo has no primary catchable type"))?;
    let thrown_end = exception
        .object_address
        .checked_add(thrown_size as u64)
        .ok_or_else(|| malformed("primary thrown object range overflows"))?;
    if matched.adjusted_object_address < exception.object_address || source_end > thrown_end {
        return Err(malformed(format!(
            "catch subobject {:#x}..{source_end:#x} escapes thrown object {:#x}..{thrown_end:#x}",
            matched.adjusted_object_address, exception.object_address
        )));
    }
    if source_end <= matched.adjusted_object_address
        || p.mem.prot_of(matched.adjusted_object_address).is_none()
        || p.mem.prot_of(source_end - 1).is_none()
    {
        return Err(malformed("thrown object range is not mapped"));
    }
    if matched.by_reference {
        p.wr64(destination, matched.adjusted_object_address)?;
    } else if let Some(copy_function) = matched.copy_function {
        p.call_guest(
            copy_function,
            &[destination, matched.adjusted_object_address],
        )?;
    } else {
        let bytes = p.read_bytes(matched.adjusted_object_address, source_size as usize)?;
        p.write_bytes(destination, &bytes)?;
    }
    Ok(destination)
}

fn unwind_funclet(p: &mut Process, action: u64, frame: u64) -> Result<(), Stop> {
    p.call_guest(action, &[0, frame])?;
    Ok(())
}

fn unwind3_to_state(
    p: &mut Process,
    info: &FuncInfo3,
    mut state: i32,
    target_state: i32,
    establisher: u64,
) -> Result<(), Stop> {
    while state > target_state {
        let entry = info
            .unwind_map
            .get(state as usize)
            .ok_or_else(|| malformed(format!("EH3 unwind state {state} is out of range")))?;
        if let Some(action) = entry.action {
            unwind_funclet(p, action, establisher).map_err(|stop| match stop {
                Stop::GuestUnwind(_) => Stop::Crash(
                    "guest exception escaped an EH3 unwind funclet during phase two".into(),
                ),
                other => other,
            })?;
        }
        state = entry.to_state;
    }
    Ok(())
}

fn unwind4_to_state(
    p: &mut Process,
    info: &FuncInfo4,
    mut state: i32,
    target_state: i32,
    establisher: u64,
) -> Result<(), Stop> {
    while state > target_state {
        let entry = info
            .unwind_map
            .get(state as usize)
            .ok_or_else(|| malformed(format!("EH4 unwind state {state} is out of range")))?;
        match entry.kind {
            UnwindEntryKind4::NoUnwind => {}
            UnwindEntryKind4::FuncletRva => {
                let action = entry
                    .action
                    .ok_or_else(|| malformed("EH4 unwind funclet has no action RVA"))?;
                unwind_funclet(p, action, establisher).map_err(|stop| match stop {
                    Stop::GuestUnwind(_) => Stop::Crash(
                        "guest exception escaped an EH4 unwind funclet during phase two".into(),
                    ),
                    other => other,
                })?;
            }
            UnwindEntryKind4::DestructorWithObject
            | UnwindEntryKind4::DestructorWithPointerToObject => {
                let action = entry
                    .action
                    .ok_or_else(|| malformed("EH4 destructor entry has no action RVA"))?;
                let displacement = entry
                    .object_displacement
                    .ok_or_else(|| malformed("EH4 destructor entry has no object displacement"))?
                    as i32 as i64;
                let object_slot = frame_address(establisher, displacement, "EH4 object")?;
                let object = if entry.kind == UnwindEntryKind4::DestructorWithPointerToObject {
                    p.rd64(object_slot)?
                } else {
                    object_slot
                };
                p.call_guest(action, &[object]).map_err(|stop| match stop {
                    Stop::GuestUnwind(_) => Stop::Crash(
                        "guest exception escaped an EH4 destructor during phase two".into(),
                    ),
                    other => other,
                })?;
            }
        }
        state = entry.to_state;
    }
    Ok(())
}

pub(crate) fn current_cpp_exception_cell(p: &mut Process, want_context: bool) -> Result<u64, Stop> {
    let thread_id = p.current_thread_id;
    if let Some(&(exception_cell, context_cell)) = p.extra.cpp_exception_cells.get(&thread_id) {
        return Ok(if want_context {
            context_cell
        } else {
            exception_cell
        });
    }
    let cells = crate::hle::kernel32::alloc_block(p, 16, true);
    p.extra
        .cpp_exception_cells
        .insert(thread_id, (cells, cells + 8));
    Ok(if want_context { cells + 8 } else { cells })
}

fn call_catch_funclet(
    p: &mut Process,
    call: HandlerCall,
    handler: u64,
    frame: u64,
    exception: &CppException,
) -> Result<(), Stop> {
    let exception_cell = current_cpp_exception_cell(p, false)?;
    let context_cell = current_cpp_exception_cell(p, true)?;
    let previous_exception = p.rd64(exception_cell)?;
    let previous_context = p.rd64(context_cell)?;
    p.wr64(exception_cell, call.exception_record)?;
    p.wr64(context_cell, call.context_record)?;
    if p.trace_imports {
        p.stderr.extend_from_slice(
            format!(
                "[cxx] call catch funclet={handler:#x} frame={frame:#x} object={:#x}\n",
                exception.object_address
            )
            .as_bytes(),
        );
    }
    let result = p.call_guest(handler, &[call.exception_record, frame]);
    let result = match result {
        Ok(continuation) => {
            if p.trace_imports {
                p.stderr.extend_from_slice(
                    format!("[cxx] catch funclet returned continuation={continuation:#x}\n")
                        .as_bytes(),
                );
            }
            if let Some(unwind_function) = exception.throw_info.unwind_function {
                match p.call_guest(unwind_function, &[exception.object_address]) {
                    Ok(_) => Ok(continuation),
                    Err(Stop::GuestUnwind(_)) => Err(Stop::Crash(
                        "guest exception escaped the thrown-object destructor".into(),
                    )),
                    Err(stop) => Err(stop),
                }
            } else {
                Ok(continuation)
            }
        }
        Err(stop) => Err(stop),
    };
    p.wr64(exception_cell, previous_exception)?;
    p.wr64(context_cell, previous_context)?;
    let continuation = result?;
    let module = module_for_image(p, call.image_base)?;
    let image_end = module
        .base
        .checked_add(module.size)
        .ok_or_else(|| malformed("catch continuation image range overflows"))?;
    if continuation < module.base
        || continuation >= image_end
        || p.mem
            .prot_of(continuation)
            .is_none_or(|protection| protection & PROT_X == 0)
    {
        return Err(malformed(format!(
            "catch funclet returned non-executable continuation {continuation:#x}"
        )));
    }
    p.wr64(call.dispatcher_context + DISPATCHER_TARGET_IP, continuation)?;
    Ok(())
}

fn resolve_eh3_catch_frame(
    p: &Process,
    call: HandlerCall,
    info: &FuncInfo3,
    state: i32,
) -> Result<u64, Stop> {
    // dispFrame is only used when execution is already inside a catch. For a
    // catch selected from a try body, the establisher itself is the frame base.
    let active_catch = info
        .try_blocks
        .iter()
        .any(|block| state > block.try_high && state <= block.catch_high);
    if !active_catch {
        return Ok(call.establisher);
    }

    let function_start = function_start_rva(p, call.image_base, call.function_entry)?;
    let handler_address = call
        .image_base
        .checked_add(function_start as u64)
        .ok_or_else(|| malformed("EH3 catch function address overflows"))?;
    let (stack_limit, stack_base) = stack_bounds(p)?;

    // Match the runtime's reverse try-block walk and select the HandlerType
    // belonging to the catch funclet that contains the control PC.
    for block in info.try_blocks.iter().rev() {
        if state <= block.try_high || state > block.catch_high {
            continue;
        }
        for handler in &block.handlers {
            if handler.handler != handler_address {
                continue;
            }
            let pointer_slot = frame_address(
                call.establisher,
                handler.frame_displacement as i64,
                "EH3 catch frame",
            )?;
            let slot_end = pointer_slot
                .checked_add(8)
                .ok_or_else(|| malformed("EH3 catch-frame pointer slot overflows"))?;
            if pointer_slot < stack_limit || slot_end > stack_base {
                return Err(malformed(format!(
                    "EH3 catch-frame pointer slot {pointer_slot:#x}..{slot_end:#x} is outside the TEB stack"
                )));
            }
            let frame = p.rd64(pointer_slot)?;
            if frame < stack_limit || frame >= stack_base {
                return Err(malformed(format!(
                    "EH3 parent frame {frame:#x} is outside the TEB stack"
                )));
            }
            return Ok(frame);
        }
    }
    Ok(call.establisher)
}

pub(crate) fn frame_handler3(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return crate::msvc_eh32::frame_handler3(p);
    }
    let call = handler_call(p)?;
    let info = parse_func_info3(p, call.image_base, call.handler_data)?;
    let state = current_state3(&info, call.image_base, call.control_pc)?;
    if call.exception_flags & EXCEPTION_UNWINDING != 0 {
        let mut target_state = -1;
        let mut selected = None;
        if call.exception_flags & EXCEPTION_TARGET_UNWIND != 0 {
            if let Some(exception) = cpp_exception_from_record(p, call.exception_record)? {
                selected = select_catch3(p, &exception, &info, state, Some(call.target_ip))?;
                if let Some((try_index, _, _)) = selected.as_ref() {
                    target_state = info.try_blocks[*try_index].try_low - 1;
                }
            }
        }
        unwind3_to_state(p, &info, state, target_state, call.establisher)?;
        if let (Some(exception), Some((try_index, handler_index, matched))) = (
            cpp_exception_from_record(p, call.exception_record)?,
            selected,
        ) {
            let handler = &info.try_blocks[try_index].handlers[handler_index];
            let frame = resolve_eh3_catch_frame(p, call, &info, state)?;
            build_catch_object(p, frame, &exception, &matched)?;
            call_catch_funclet(p, call, handler.handler, frame, &exception)?;
        }
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    }

    let Some(exception) = cpp_exception_from_record(p, call.exception_record)? else {
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    };
    let selected = select_catch3(p, &exception, &info, state, None)?;
    if p.trace_imports {
        let active = active_try_indices(&info.try_blocks, state, |block| {
            (block.try_low, block.try_high)
        })
        .into_iter()
        .map(|index| {
            (
                index,
                info.try_blocks[index].try_low,
                info.try_blocks[index].try_high,
            )
        })
        .collect::<Vec<_>>();
        p.stderr.extend_from_slice(
            format!(
                "[cxx] FH3 pc={:#x} state={state} active={active:?} selected={selected:?}\n",
                call.control_pc
            )
            .as_bytes(),
        );
    }
    let Some((try_index, handler_index, _matched)) = selected else {
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    };
    let handler = &info.try_blocks[try_index].handlers[handler_index];
    let module = module_for_image(p, call.image_base)?;
    if handler.handler < module.base
        || handler.handler >= module.base + module.size
        || p.mem
            .prot_of(handler.handler)
            .is_none_or(|prot| prot & PROT_X == 0)
    {
        return Err(malformed(format!(
            "EH3 catch handler {:#x} is not executable guest code",
            handler.handler
        )));
    }
    p.wr32(
        call.dispatcher_context + DISPATCHER_SCOPE_INDEX,
        handler_index as u32 + 1,
    )?;
    Err(Stop::GuestUnwind(GuestUnwind {
        target_frame: call.establisher,
        target_ip: handler.handler,
        call_rsp: p.cpu.gpr[RSP],
        exception_record: call.exception_record,
        return_value: 0,
        context_record: call.context_record,
        history_table: call.history_table,
    }))
}

pub(crate) fn frame_handler4(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: "PE32 __CxxFrameHandler4".into(),
        });
    }
    let call = handler_call(p)?;
    let info = parse_func_info4(p, call.image_base, call.handler_data)?;
    let state = current_state4(
        p,
        &info,
        call.image_base,
        call.function_entry,
        call.control_pc,
    )?;
    if call.exception_flags & EXCEPTION_UNWINDING != 0 {
        let mut target_state = -1;
        let mut selected = None;
        if call.exception_flags & EXCEPTION_TARGET_UNWIND != 0 {
            if let Some(exception) = cpp_exception_from_record(p, call.exception_record)? {
                selected = select_catch4(p, &exception, &info, state, Some(call.target_ip))?;
                if let Some((try_index, _, _)) = selected.as_ref() {
                    target_state = info.try_blocks[*try_index].try_low - 1;
                }
            }
        }
        unwind4_to_state(p, &info, state, target_state, call.establisher)?;
        if let (Some(exception), Some((try_index, handler_index, matched))) = (
            cpp_exception_from_record(p, call.exception_record)?,
            selected,
        ) {
            let handler = &info.try_blocks[try_index].handlers[handler_index];
            build_catch_object(p, call.establisher, &exception, &matched)?;
            call_catch_funclet(p, call, handler.handler, call.establisher, &exception)?;
        }
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    }

    let Some(exception) = cpp_exception_from_record(p, call.exception_record)? else {
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    };
    let selected = select_catch4(p, &exception, &info, state, None)?;
    if p.trace_imports {
        let active = active_try_indices(&info.try_blocks, state, |block| {
            (block.try_low, block.try_high)
        })
        .into_iter()
        .map(|index| {
            (
                index,
                info.try_blocks[index].try_low,
                info.try_blocks[index].try_high,
            )
        })
        .collect::<Vec<_>>();
        p.stderr.extend_from_slice(
            format!(
                "[cxx] FH4 pc={:#x} function_entry={:#x} state={state} active={active:?} tries={:?} ipmap={:?} selected={selected:?}\n",
                call.control_pc, call.function_entry, info.try_blocks, info.ip_to_state
            )
            .as_bytes(),
        );
    }
    let Some((try_index, handler_index, _matched)) = selected else {
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    };
    let handler = &info.try_blocks[try_index].handlers[handler_index];
    let module = module_for_image(p, call.image_base)?;
    if handler.handler < module.base
        || handler.handler >= module.base + module.size
        || p.mem
            .prot_of(handler.handler)
            .is_none_or(|prot| prot & PROT_X == 0)
    {
        return Err(malformed(format!(
            "EH4 catch handler {:#x} is not executable guest code",
            handler.handler
        )));
    }
    p.wr32(
        call.dispatcher_context + DISPATCHER_SCOPE_INDEX,
        handler_index as u32 + 1,
    )?;
    Err(Stop::GuestUnwind(GuestUnwind {
        target_frame: call.establisher,
        target_ip: handler.handler,
        call_rsp: p.cpu.gpr[RSP],
        exception_record: call.exception_record,
        return_value: 0,
        context_record: call.context_record,
        history_table: call.history_table,
    }))
}

pub(crate) fn cxx_throw_exception(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode == CpuMode::X86_32 {
        return crate::msvc_eh32::cxx_throw_exception(p);
    }
    if p.cpu.mode != CpuMode::X86_64 {
        return Err(Stop::Unimplemented {
            dll: "vcruntime".into(),
            name: "_CxxThrowException for PE32".into(),
        });
    }
    let object_address = p.arg(0);
    let throw_info = p.arg(1);
    let image_base = p
        .modules
        .iter()
        .find(|module| {
            module.hle_group.is_none()
                && throw_info >= module.base
                && throw_info < module.base.saturating_add(module.size)
        })
        .map(|module| module.base)
        .ok_or_else(|| {
            malformed(format!(
                "_CxxThrowException ThrowInfo {throw_info:#x} is outside loaded PE images"
            ))
        })?;
    let caller = p.rd64(p.cpu.gpr[RSP])?;
    crate::exception::dispatch_raised_exception(
        p,
        MSVC_CPP_EXCEPTION,
        EXCEPTION_NONCONTINUABLE,
        &[MSVC_CPP_MAGIC, object_address, throw_info, image_base],
        caller,
    )
}

pub(crate) fn std_terminate(_p: &mut Process) -> Result<u64, Stop> {
    Err(Stop::Crash(
        "std::terminate was called during guest C++ exception processing".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Module, Process};
    use crate::vfs::Vfs;
    use anywasm_cpu::mem::{PROT_RW, PROT_RWX};

    const BASE: u64 = 0x1400_0000_0;
    const IMAGE_SIZE: u64 = 0x10_000;
    const TEST_EXCEPTION_RECORD: u64 = BASE + 0xa000;
    const TEST_CONTEXT_RECORD: u64 = BASE + 0xa200;
    const TEST_DISPATCHER_CONTEXT: u64 = BASE + 0xa300;
    const TEST_ESTABLISHER: u64 = 0x74_000;

    fn process_with_image() -> Process {
        let mut p = Process::new(Vfs::new());
        assert!(p.mem.map(BASE, IMAGE_SIZE, PROT_RW));
        p.modules.push(Module {
            name: "fixture.exe".into(),
            path: "C:\\fixture.exe".into(),
            base: BASE,
            size: IMAGE_SIZE,
            entry: BASE + 0x1000,
            is_dll: false,
            tls_callbacks: Vec::new(),
            exports: Default::default(),
            ordinals: Default::default(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: 0,
            resource_size: 0,
            hle_group: None,
        });
        p
    }

    fn map_test_stack(p: &mut Process) -> (u64, u64) {
        let stack_limit = 0x70_000;
        let stack_base = 0x80_000;
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        assert!(p.mem.map(stack_limit, stack_base - stack_limit, PROT_RW));
        p.wr64(TEB_ADDR + 0x08, stack_base).unwrap();
        p.wr64(TEB_ADDR + 0x10, stack_limit).unwrap();
        (stack_limit, stack_base)
    }

    fn setup_frame_handler_call(
        p: &mut Process,
        handler_data: u64,
        control_pc: u64,
        target_ip: u64,
        exception_flags: u32,
    ) -> [u64; 4] {
        let (_, stack_base) = map_test_stack(p);
        p.cpu.mode = CpuMode::X86_64;
        p.cpu.gpr[RSP] = stack_base - 0x400;
        assert!(p.mem.protect(BASE, IMAGE_SIZE, PROT_RWX));

        p.wr32(TEST_EXCEPTION_RECORD, MSVC_CPP_EXCEPTION).unwrap();
        p.wr32(TEST_EXCEPTION_RECORD + 4, exception_flags)
            .unwrap();
        p.wr64(TEST_EXCEPTION_RECORD + 8, 0).unwrap();
        p.wr64(TEST_EXCEPTION_RECORD + 16, BASE + 0x1100)
            .unwrap();
        p.wr32(TEST_EXCEPTION_RECORD + EXCEPTION_RECORD_PARAMETERS, 4)
            .unwrap();
        p.wr64(TEST_EXCEPTION_RECORD + EXCEPTION_RECORD_ARGUMENTS, MSVC_CPP_MAGIC)
            .unwrap();
        p.wr64(TEST_EXCEPTION_RECORD + EXCEPTION_RECORD_ARGUMENTS + 8, BASE + 0x8000)
            .unwrap();
        p.wr64(TEST_EXCEPTION_RECORD + EXCEPTION_RECORD_ARGUMENTS + 16, BASE + 0x600)
            .unwrap();
        p.wr64(TEST_EXCEPTION_RECORD + EXCEPTION_RECORD_ARGUMENTS + 24, BASE)
            .unwrap();

        p.wr32(BASE + 0x1200, 0x1000).unwrap(); // runtime-function begin RVA
        p.wr32(BASE + 0x1204, 0x1100).unwrap(); // runtime-function end RVA
        p.wr32(BASE + 0x1208, 0).unwrap(); // unwind-data RVA
        for (offset, value) in [
            (DISPATCHER_CONTROL_PC, control_pc),
            (DISPATCHER_IMAGE_BASE, BASE),
            (DISPATCHER_FUNCTION_ENTRY, BASE + 0x1200),
            (DISPATCHER_TARGET_IP, target_ip),
            (DISPATCHER_HANDLER_DATA, handler_data),
            (DISPATCHER_HISTORY_TABLE, 0),
        ] {
            p.wr64(TEST_DISPATCHER_CONTEXT + offset, value).unwrap();
        }
        p.wr32(TEST_DISPATCHER_CONTEXT + DISPATCHER_SCOPE_INDEX, 0x55)
            .unwrap();

        [
            TEST_EXCEPTION_RECORD,
            TEST_ESTABLISHER,
            TEST_CONTEXT_RECORD,
            TEST_DISPATCHER_CONTEXT,
        ]
    }

    fn guest_return_u64(p: &mut Process, address: u64, value: u64) {
        let mut code = vec![0x48, 0xb8]; // mov rax, imm64
        code.extend_from_slice(&value.to_le_bytes());
        code.push(0xc3); // ret
        p.write_bytes(address, &code).unwrap();
    }

    fn guest_store_u32(p: &mut Process, address: u64, destination: u64, value: u32) {
        let mut code = vec![0x48, 0xb8]; // mov rax, imm64
        code.extend_from_slice(&destination.to_le_bytes());
        code.extend_from_slice(&[0xc7, 0x00]); // mov dword ptr [rax], imm32
        code.extend_from_slice(&value.to_le_bytes());
        code.push(0xc3); // ret
        p.write_bytes(address, &code).unwrap();
    }

    fn put_u32(p: &mut Process, rva: u64, value: u32) {
        p.wr32(BASE + rva, value).unwrap();
    }

    fn put_i32(p: &mut Process, rva: u64, value: i32) {
        put_u32(p, rva, value as u32);
    }

    fn put_bytes(p: &mut Process, rva: u64, bytes: &[u8]) {
        p.write_bytes(BASE + rva, bytes).unwrap();
    }

    fn put_type_descriptor(p: &mut Process, rva: u64, name: &[u8]) {
        put_bytes(p, rva, &[0; 16]);
        put_bytes(p, rva + 16, name);
        put_bytes(p, rva + 16 + name.len() as u64, &[0]);
    }

    fn write_valid_func_info3(p: &mut Process) {
        // HandlerData contains an image-base-relative displacement to FuncInfo.
        put_i32(p, 0x100, 0x200);
        put_u32(p, 0x200, 0x1993_0522);
        put_i32(p, 0x204, 1); // maxState
        put_i32(p, 0x208, 0x300); // unwind map
        put_u32(p, 0x20c, 1); // try blocks
        put_i32(p, 0x210, 0x310); // try block map
        put_u32(p, 0x214, 1); // IP map entries
        put_i32(p, 0x218, 0x330); // IP-to-state map
        put_i32(p, 0x21c, 0); // unwind help
        put_i32(p, 0x220, 0); // exception specification
        put_i32(p, 0x224, 0); // EH flags

        put_i32(p, 0x300, -1);
        put_i32(p, 0x304, 0x400);

        put_i32(p, 0x310, 0); // tryLow
        put_i32(p, 0x314, 0); // tryHigh
        put_i32(p, 0x318, 0); // catchHigh
        put_u32(p, 0x31c, 1); // catches
        put_i32(p, 0x320, 0x350); // handler array

        put_u32(p, 0x350, 0); // adjectives
        put_i32(p, 0x354, 0x380); // type descriptor
        put_i32(p, 0x358, -8); // catch object displacement
        put_i32(p, 0x35c, 0x500); // handler RVA
        put_i32(p, 0x360, 0); // frame displacement
        put_type_descriptor(p, 0x380, b".?AUDerived@@");

        put_u32(p, 0x330, 0x700);
        put_i32(p, 0x334, 0);
    }

    fn write_valid_throw_info(p: &mut Process) {
        put_u32(p, 0x600, 0); // attributes
        put_i32(p, 0x604, 0x700); // unwind function
        put_i32(p, 0x608, 0); // forward-compatibility function
        put_i32(p, 0x60c, 0x620); // catchable type array

        put_i32(p, 0x620, 1); // count
        put_i32(p, 0x624, 0x640); // catchable type pointer

        put_u32(p, 0x640, 0); // properties
        put_i32(p, 0x644, 0x680); // type descriptor
        put_i32(p, 0x648, 0); // PMD member displacement
        put_i32(p, 0x64c, -1); // PMD vbtable displacement
        put_i32(p, 0x650, 0); // PMD vbase displacement
        put_u32(p, 0x654, 16); // object size
        put_i32(p, 0x658, 0); // trivial/simple copy
        put_type_descriptor(p, 0x680, b".?AUDerived@@");
    }

    fn catchable_type(
        name: &str,
        member_displacement: i32,
        vbtable_displacement: i32,
        properties: u32,
        copy_function: Option<u64>,
    ) -> CatchableType {
        CatchableType {
            properties,
            type_descriptor: TypeDescriptor {
                address: BASE + 0x700,
                decorated_name: name.into(),
            },
            this_displacement: Pmd {
                member_displacement,
                vbtable_displacement,
                vbase_displacement: 0,
            },
            size_or_offset: 16,
            copy_function,
        }
    }

    fn cpp_exception(attributes: u32, catchable_types: Vec<CatchableType>) -> CppException {
        CppException {
            object_address: BASE + 0x8000,
            throw_info: ThrowInfo {
                attributes,
                unwind_function: None,
                forward_compatibility: None,
                catchable_types,
            },
        }
    }

    fn typed_handler3(name: &str, adjectives: u32) -> HandlerType3 {
        HandlerType3 {
            adjectives,
            type_descriptor: Some(TypeDescriptor {
                address: BASE + 0x780,
                decorated_name: name.into(),
            }),
            catch_object_displacement: -8,
            handler: BASE + 0x900,
            frame_displacement: 0,
        }
    }

    #[test]
    fn eh3_uses_establisher_for_try_catches_and_disp_frame_inside_catch() {
        let mut p = process_with_image();
        let stack_limit = 0x70_000;
        let stack_base = 0x80_000;
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        assert!(p.mem.map(stack_limit, stack_base - stack_limit, PROT_RW));
        p.wr64(TEB_ADDR + 0x08, stack_base).unwrap();
        p.wr64(TEB_ADDR + 0x10, stack_limit).unwrap();

        let establisher = 0x71_000;
        let frame_displacement = 0x20i32;
        let expected_frame = 0x74_000;
        p.wr64(establisher + frame_displacement as u64, expected_frame)
            .unwrap();
        p.wr32(BASE + 0x900, 0x1000).unwrap(); // runtime-function begin RVA
        let handler = HandlerType3 {
            handler: BASE + 0x1000,
            frame_displacement,
            ..typed_handler3(".?AUDerived@@", 0)
        };
        let info = FuncInfo3 {
            magic: 0x1993_0522,
            max_state: 2,
            unwind_map: Vec::new(),
            try_blocks: vec![TryBlockMapEntry3 {
                try_low: 0,
                try_high: 0,
                catch_high: 1,
                handlers: vec![handler],
            }],
            ip_to_state: Vec::new(),
            unwind_help: None,
            exception_specification: None,
            eh_flags: 0,
        };
        let call = HandlerCall {
            exception_record: 0,
            establisher,
            context_record: 0,
            dispatcher_context: 0,
            exception_flags: 0,
            control_pc: BASE + 0x1000,
            image_base: BASE,
            function_entry: BASE + 0x900,
            target_ip: 0,
            handler_data: 0,
            history_table: 0,
        };

        assert_eq!(
            resolve_eh3_catch_frame(&p, call, &info, 0).unwrap(),
            establisher
        );
        assert_eq!(
            resolve_eh3_catch_frame(&p, call, &info, 1).unwrap(),
            expected_frame
        );
    }

    #[test]
    fn frame_handler3_thunk_selects_a_typed_handler_during_search() {
        let mut p = process_with_image();
        write_valid_func_info3(&mut p);
        write_valid_throw_info(&mut p);
        p.write_bytes(BASE + 0x8000, &[0x5a; 16]).unwrap();
        let args = setup_frame_handler_call(&mut p, BASE + 0x100, BASE + 0x700, 0, 0);
        let exception_before = p.read_bytes(TEST_EXCEPTION_RECORD, 0x98).unwrap();
        let thunk = p.thunk_for("crt", "__CxxFrameHandler3", true).unwrap();

        let result = p.call_guest(thunk, &args);
        let Err(Stop::GuestUnwind(unwind)) = result else {
            panic!("typed EH3 search should select its guest catch handler: {result:?}");
        };
        assert_eq!(unwind.target_frame, TEST_ESTABLISHER);
        assert_eq!(unwind.target_ip, BASE + 0x500);
        assert_eq!(unwind.exception_record, TEST_EXCEPTION_RECORD);
        assert_eq!(unwind.context_record, TEST_CONTEXT_RECORD);
        assert_eq!(p.rd32(TEST_DISPATCHER_CONTEXT + DISPATCHER_SCOPE_INDEX).unwrap(), 1);
        assert_eq!(
            p.read_bytes(TEST_EXCEPTION_RECORD, 0x98).unwrap(),
            exception_before
        );
    }

    #[test]
    fn frame_handler4_thunk_selects_a_catch_all_during_search() {
        let mut p = process_with_image();
        write_valid_func_info4(&mut p);
        // A zero HandlerType header means no adjectives, type descriptor, or catch object.
        p.wr8(BASE + 0xd01, 0).unwrap();
        p.wr32(BASE + 0xd02, 0xf00).unwrap();
        write_valid_throw_info(&mut p);
        p.write_bytes(BASE + 0x8000, &[0xa5; 16]).unwrap();
        let args = setup_frame_handler_call(&mut p, BASE + 0x900, BASE + 0x1000, 0, 0);
        let thunk = p.thunk_for("crt", "__CxxFrameHandler4", true).unwrap();

        let result = p.call_guest(thunk, &args);
        let Err(Stop::GuestUnwind(unwind)) = result else {
            panic!("catch-all EH4 search should select its guest catch handler: {result:?}");
        };
        assert_eq!(unwind.target_frame, TEST_ESTABLISHER);
        assert_eq!(unwind.target_ip, BASE + 0xf00);
        assert_eq!(unwind.exception_record, TEST_EXCEPTION_RECORD);
        assert_eq!(unwind.context_record, TEST_CONTEXT_RECORD);
        assert_eq!(p.rd32(TEST_DISPATCHER_CONTEXT + DISPATCHER_SCOPE_INDEX).unwrap(), 1);
    }

    #[test]
    fn frame_handler3_thunk_continues_search_without_changing_an_unmatched_exception() {
        let mut p = process_with_image();
        write_valid_func_info3(&mut p);
        write_valid_throw_info(&mut p);
        put_type_descriptor(&mut p, 0x380, b".?AUMissing@@");
        p.write_bytes(BASE + 0x8000, &[0x3c; 16]).unwrap();
        let args = setup_frame_handler_call(&mut p, BASE + 0x100, BASE + 0x700, 0, 0);
        let exception_before = p.read_bytes(TEST_EXCEPTION_RECORD, 0x98).unwrap();
        let thunk = p.thunk_for("crt", "__CxxFrameHandler3", true).unwrap();

        assert_eq!(p.call_guest(thunk, &args).unwrap(), EXCEPTION_CONTINUE_SEARCH);
        assert_eq!(
            p.read_bytes(TEST_EXCEPTION_RECORD, 0x98).unwrap(),
            exception_before
        );
        assert_eq!(p.rd32(TEST_DISPATCHER_CONTEXT + DISPATCHER_SCOPE_INDEX).unwrap(), 0x55);
    }

    #[test]
    fn frame_handler3_target_unwind_runs_the_destructor_and_catch_funclet() {
        let mut p = process_with_image();
        write_valid_func_info3(&mut p);
        write_valid_throw_info(&mut p);
        put_i32(&mut p, 0x604, 0); // the thrown object itself has no unwind function
        let object_bytes = [0x69; 16];
        p.write_bytes(BASE + 0x8000, &object_bytes).unwrap();
        let marker = BASE + 0xa400;
        guest_store_u32(&mut p, BASE + 0x400, marker, 1);
        let continuation = BASE + 0x1600;
        guest_return_u64(&mut p, BASE + 0x500, continuation);
        let flags = EXCEPTION_NONCONTINUABLE | EXCEPTION_UNWINDING | EXCEPTION_TARGET_UNWIND;
        let args = setup_frame_handler_call(
            &mut p,
            BASE + 0x100,
            BASE + 0x700,
            BASE + 0x500,
            flags,
        );
        let exception_before = p.read_bytes(TEST_EXCEPTION_RECORD, 0x98).unwrap();
        let thunk = p.thunk_for("crt", "__CxxFrameHandler3", true).unwrap();

        assert_eq!(p.call_guest(thunk, &args).unwrap(), EXCEPTION_CONTINUE_SEARCH);
        assert_eq!(p.rd32(marker).unwrap(), 1);
        assert_eq!(
            p.read_bytes(TEST_ESTABLISHER - 8, object_bytes.len())
                .unwrap(),
            object_bytes
        );
        assert_eq!(
            p.rd64(TEST_DISPATCHER_CONTEXT + DISPATCHER_TARGET_IP)
                .unwrap(),
            continuation
        );
        assert_eq!(p.rd32(TEST_EXCEPTION_RECORD + 4).unwrap(), flags);
        assert_eq!(
            p.read_bytes(TEST_EXCEPTION_RECORD, 0x98).unwrap(),
            exception_before
        );
    }

    fn write_valid_func_info4(p: &mut Process) {
        // HandlerData points to the image-relative FuncInfo4 record.
        put_i32(p, 0x900, 0x200);
        put_bytes(p, 0x200, &[0x18]); // UnwindMap | TryBlockMap
        put_i32(p, 0x201, 0xa00);
        put_i32(p, 0x205, 0xb00);
        put_i32(p, 0x209, 0xc00);

        // States 0 and 1 have no destructor and unwind to -1 and 0. The packed first
        // field is (one byte back << 2) | NoUnwind == 4, encoded as a one-byte value 8.
        put_bytes(p, 0xa00, &[0x04, 0x08, 0x08]);

        // One try block for state 0, with catch state 1 and a single typed handler.
        // Unlike IP-map states, these three fields encode their values directly.
        put_bytes(p, 0xb00, &[0x02, 0x00, 0x00, 0x02]);
        put_i32(p, 0xb04, 0xd00);
        put_bytes(p, 0xd00, &[0x02, 0x02]); // count=1, dispType present
        put_i32(p, 0xd02, 0xe00);
        put_i32(p, 0xd06, 0xf00);
        put_type_descriptor(p, 0xe00, b".?AUDerived@@");

        // One IP delta (0) maps to state 0 (encoded as state + 1).
        put_bytes(p, 0xc00, &[0x02, 0x00, 0x02]);
    }

    #[test]
    fn parses_valid_eh3_function_tables_and_handler_type() {
        let mut p = process_with_image();
        write_valid_func_info3(&mut p);

        let info = parse_func_info3(&p, BASE, BASE + 0x100).unwrap();
        assert_eq!(info.magic, 0x1993_0522);
        assert_eq!(info.max_state, 1);
        assert_eq!(info.unwind_map[0].to_state, -1);
        assert_eq!(info.try_blocks[0].handlers[0].handler, BASE + 0x500);
        assert_eq!(
            info.try_blocks[0].handlers[0]
                .type_descriptor
                .as_ref()
                .unwrap()
                .decorated_name,
            ".?AUDerived@@"
        );
        assert_eq!(info.ip_to_state[0].ip_rva, 0x700);
    }

    #[test]
    fn rejects_eh3_tables_outside_the_owning_image() {
        let mut p = process_with_image();
        write_valid_func_info3(&mut p);
        put_i32(&mut p, 0x208, IMAGE_SIZE as i32 - 4);

        assert!(matches!(
            parse_func_info3(&p, BASE, BASE + 0x100),
            Err(Stop::Crash(_))
        ));
    }

    #[test]
    fn rejects_eh3_counts_that_overflow_the_supported_table_limit() {
        let mut p = process_with_image();
        write_valid_func_info3(&mut p);
        put_i32(&mut p, 0x204, i32::MAX);

        assert!(matches!(
            parse_func_info3(&p, BASE, BASE + 0x100),
            Err(Stop::Crash(_))
        ));
    }

    #[test]
    fn parses_valid_throw_info_and_catchable_type_array() {
        let mut p = process_with_image();
        write_valid_throw_info(&mut p);

        let info = parse_throw_info(&p, BASE, BASE + 0x600).unwrap();
        assert_eq!(info.catchable_types.len(), 1);
        assert_eq!(
            info.catchable_types[0].type_descriptor.decorated_name,
            ".?AUDerived@@"
        );
        assert_eq!(info.catchable_types[0].size_or_offset, 16);
        assert_eq!(
            info.catchable_types[0]
                .this_displacement
                .vbtable_displacement,
            -1
        );
    }

    #[test]
    fn rejects_malformed_throw_type_descriptor_name() {
        let mut p = process_with_image();
        write_valid_throw_info(&mut p);
        // Replace the terminator with a non-ASCII byte; malformed names fail closed.
        put_bytes(&mut p, 0x680 + 16, &[0xff]);

        assert!(matches!(
            parse_throw_info(&p, BASE, BASE + 0x600),
            Err(Stop::Crash(_))
        ));
    }

    #[test]
    fn matches_exact_types_for_value_and_reference_catches() {
        let p = process_with_image();
        let exception = cpp_exception(
            0,
            vec![catchable_type(
                ".?AUDerived@@",
                0,
                -1,
                0,
                Some(BASE + 0x500),
            )],
        );

        let by_value = match_handler3(&p, &exception, &typed_handler3(".?AUDerived@@", 0))
            .unwrap()
            .unwrap();
        assert!(!by_value.by_reference);
        assert_eq!(by_value.copy_function, Some(BASE + 0x500));
        assert_eq!(by_value.adjusted_object_address, exception.object_address);

        let by_reference = match_handler3(
            &p,
            &exception,
            &typed_handler3(".?AUDerived@@", HT_IS_REFERENCE),
        )
        .unwrap()
        .unwrap();
        assert!(by_reference.by_reference);
        assert_eq!(by_reference.copy_function, None);
    }

    #[test]
    fn builds_value_catches_and_adjusted_base_reference_catches() {
        let mut p = process_with_image();
        let (_, stack_base) = map_test_stack(&mut p);
        let object_address = BASE + 0x8000;
        let source = (0u8..16).collect::<Vec<_>>();
        p.write_bytes(object_address, &source).unwrap();
        let mut exception = cpp_exception(
            0,
            vec![
                catchable_type(".?AUDerived@@", 0, -1, 0, None),
                catchable_type(".?AUBase@@", 4, -1, 0, None),
            ],
        );
        exception.throw_info.catchable_types[1].size_or_offset = 4;

        let value_handler = HandlerType3 {
            catch_object_displacement: 0x20,
            ..typed_handler3(".?AUDerived@@", 0)
        };
        let value_match = match_handler3(&p, &exception, &value_handler)
            .unwrap()
            .unwrap();
        let frame = stack_base - 0x300;
        let value_address = build_catch_object(&mut p, frame, &exception, &value_match).unwrap();
        assert_eq!(value_address, frame + 0x20);
        assert_eq!(p.read_bytes(value_address, source.len()).unwrap(), source);

        let reference_handler = HandlerType3 {
            catch_object_displacement: 0x40,
            ..typed_handler3(".?AUBase@@", HT_IS_REFERENCE)
        };
        let reference_match = match_handler3(&p, &exception, &reference_handler)
            .unwrap()
            .unwrap();
        let reference_address =
            build_catch_object(&mut p, frame, &exception, &reference_match).unwrap();
        assert_eq!(reference_address, frame + 0x40);
        assert_eq!(p.rd64(reference_address).unwrap(), object_address + 4);
    }

    #[test]
    fn by_value_catch_invokes_its_guest_copy_function() {
        let mut p = process_with_image();
        let (_, stack_base) = map_test_stack(&mut p);
        let object_address = BASE + 0x8000;
        let source = (0xa0u8..0xb0).collect::<Vec<_>>();
        p.write_bytes(object_address, &source).unwrap();
        let copy_function = BASE + 0x1000;
        assert!(p
            .mem
            .protect(copy_function, 0x1000, anywasm_cpu::mem::PROT_RWX));
        p.write_bytes(
            copy_function,
            &[
                0x48, 0x8b, 0x02, // mov rax, [rdx]
                0x48, 0x89, 0x01, // mov [rcx], rax
                0x48, 0x8b, 0x42, 0x08, // mov rax, [rdx + 8]
                0x48, 0x89, 0x41, 0x08, // mov [rcx + 8], rax
                0x31, 0xc0, // xor eax, eax
                0xc3, // ret
            ],
        )
        .unwrap();
        p.cpu.gpr[RSP] = stack_base - 0x800;

        let exception = cpp_exception(
            0,
            vec![catchable_type(
                ".?AUDerived@@",
                0,
                -1,
                0,
                Some(copy_function),
            )],
        );
        let handler = HandlerType3 {
            catch_object_displacement: 0x20,
            ..typed_handler3(".?AUDerived@@", 0)
        };
        let matched = match_handler3(&p, &exception, &handler).unwrap().unwrap();
        let destination =
            build_catch_object(&mut p, stack_base - 0x300, &exception, &matched).unwrap();
        assert_eq!(
            p.read_bytes(destination, source.len()).unwrap(),
            source,
            "the guest copy function must populate the catch buffer"
        );
    }

    #[test]
    fn rejects_catch_object_offsets_outside_the_teb_stack() {
        let mut p = process_with_image();
        let (_, stack_base) = map_test_stack(&mut p);
        let object_address = BASE + 0x8000;
        p.write_bytes(object_address, &[1; 16]).unwrap();
        let exception = cpp_exception(0, vec![catchable_type(".?AUDerived@@", 0, -1, 0, None)]);
        let handler = HandlerType3 {
            catch_object_displacement: 16,
            ..typed_handler3(".?AUDerived@@", 0)
        };
        let matched = match_handler3(&p, &exception, &handler).unwrap().unwrap();

        assert!(matches!(
            build_catch_object(&mut p, stack_base - 4, &exception, &matched),
            Err(Stop::Crash(_))
        ));
    }

    #[test]
    fn adjusts_a_base_reference_using_its_nonvirtual_pmd() {
        let p = process_with_image();
        let exception = cpp_exception(
            0,
            vec![
                catchable_type(".?AUDerived@@", 0, -1, 0, None),
                catchable_type(".?AUBase@@", 4, -1, 0, None),
            ],
        );

        let matched = match_handler3(
            &p,
            &exception,
            &typed_handler3(".?AUBase@@", HT_IS_REFERENCE),
        )
        .unwrap()
        .unwrap();
        assert_eq!(matched.catchable_type_index, Some(1));
        assert_eq!(
            matched.adjusted_object_address,
            exception.object_address + 4
        );
    }

    #[test]
    fn applies_reference_qualifiers_and_by_reference_only_rules() {
        let p = process_with_image();
        let exception = cpp_exception(
            TI_IS_CONST,
            vec![catchable_type(
                ".?AUDerived@@",
                0,
                -1,
                CT_BY_REFERENCE_ONLY,
                None,
            )],
        );

        assert!(
            match_handler3(&p, &exception, &typed_handler3(".?AUDerived@@", 0))
                .unwrap()
                .is_none()
        );
        assert!(match_handler3(
            &p,
            &exception,
            &typed_handler3(".?AUDerived@@", HT_IS_REFERENCE),
        )
        .unwrap()
        .is_none());
        assert!(match_handler3(
            &p,
            &exception,
            &typed_handler3(".?AUDerived@@", HT_IS_REFERENCE | HT_IS_CONST),
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn matches_catch_all_for_native_cpp_exceptions() {
        let p = process_with_image();
        let exception = cpp_exception(0, vec![catchable_type("int", 0, -1, 0, None)]);
        let handler = HandlerType4 {
            adjectives: HT_IS_STD_DOTDOT,
            type_descriptor: None,
            catch_object_displacement: None,
            handler: BASE + 0x900,
            continuation_addresses: Vec::new(),
        };

        let matched = match_handler4(&p, &exception, &handler).unwrap().unwrap();
        assert!(matched.is_catch_all);
        assert_eq!(matched.adjusted_object_address, exception.object_address);
    }

    #[test]
    fn rejects_virtual_base_catch_conversion() {
        let p = process_with_image();
        let exception = cpp_exception(
            0,
            vec![catchable_type(
                ".?AUBase@@",
                4,
                8,
                CT_HAS_VIRTUAL_BASE,
                None,
            )],
        );

        assert!(matches!(
            match_handler3(
                &p,
                &exception,
                &typed_handler3(".?AUBase@@", HT_IS_REFERENCE),
            ),
            Err(Stop::Unimplemented { .. })
        ));
    }

    #[test]
    fn pure_cpp_exceptions_do_not_match_native_catch_all() {
        let p = process_with_image();
        let exception = cpp_exception(TI_IS_PURE, vec![catchable_type("int", 0, -1, 0, None)]);
        let handler = CatchHandler {
            adjectives: HT_IS_STD_DOTDOT,
            decorated_type_name: None,
            catch_object_displacement: 0,
        };

        assert!(match_handler(&p, &exception, &handler).unwrap().is_none());
    }

    #[test]
    fn rejects_a_func_info_root_outside_the_image() {
        let mut p = process_with_image();
        put_i32(&mut p, 0x100, IMAGE_SIZE as i32 - 2);

        assert!(matches!(
            parse_func_info3(&p, BASE, BASE + 0x100),
            Err(Stop::Crash(_))
        ));
    }

    #[test]
    fn parses_eh4_unwind_try_handler_and_ip_maps() {
        let mut p = process_with_image();
        write_valid_func_info4(&mut p);

        let info = parse_func_info4(&p, BASE, BASE + 0x900).unwrap();
        assert_eq!(info.unwind_map.len(), 2);
        assert_eq!(info.unwind_map[0].to_state, -1);
        assert_eq!(info.unwind_map[0].kind, UnwindEntryKind4::NoUnwind);
        assert_eq!(info.unwind_map[1].to_state, 0);
        assert_eq!(info.try_blocks[0].try_low, 0);
        assert_eq!(info.try_blocks[0].try_high, 0);
        assert_eq!(info.try_blocks[0].catch_high, 1);
        assert_eq!(info.try_blocks[0].handlers[0].handler, BASE + 0xf00);
        assert_eq!(
            info.try_blocks[0].handlers[0]
                .type_descriptor
                .as_ref()
                .unwrap()
                .decorated_name,
            ".?AUDerived@@"
        );
        assert_eq!(info.ip_to_state[0].entries[0].function_relative_ip, 0);
        assert_eq!(info.ip_to_state[0].entries[0].state, 0);
    }

    #[test]
    fn parses_eh4_optional_bbt_and_catch_funclet_fields() {
        let mut p = process_with_image();
        put_i32(&mut p, 0x900, 0x200);
        put_bytes(&mut p, 0x200, &[0x05, 0x02]); // isCatch | BBT, flags=1
        put_i32(&mut p, 0x202, 0); // no IP map
        put_bytes(&mut p, 0x206, &[0x06]); // frame displacement=3

        let info = parse_func_info4(&p, BASE, BASE + 0x900).unwrap();
        assert_eq!(info.bbt_flags, Some(1));
        assert_eq!(info.frame_displacement, Some(3));
        assert!(info.unwind_map.is_empty());
        assert!(info.try_blocks.is_empty());
    }

    #[test]
    fn parses_separated_eh4_ip_state_segments() {
        let mut p = process_with_image();
        put_i32(&mut p, 0x900, 0x200);
        put_bytes(&mut p, 0x200, &[0x02]); // isSeparated
        put_i32(&mut p, 0x201, 0x1100);
        put_bytes(&mut p, 0x1100, &[0x02]); // one segment
        put_u32(&mut p, 0x1101, 0x1000);
        put_i32(&mut p, 0x1105, 0x1200);
        put_bytes(&mut p, 0x1200, &[0x02, 0x08, 0x00]); // IP +4, state -1

        let info = parse_func_info4(&p, BASE, BASE + 0x900).unwrap();
        assert!(info.is_separated);
        assert_eq!(info.ip_to_state.len(), 1);
        assert_eq!(info.ip_to_state[0].function_start_rva, Some(0x1000));
        assert_eq!(info.ip_to_state[0].entries[0].function_relative_ip, 4);
        assert_eq!(info.ip_to_state[0].entries[0].state, -1);
    }

    #[test]
    fn rejects_eh4_reserved_header_bits() {
        let mut p = process_with_image();
        put_i32(&mut p, 0x900, 0x200);
        put_bytes(&mut p, 0x200, &[0x80]);

        assert!(matches!(
            parse_func_info4(&p, BASE, BASE + 0x900),
            Err(Stop::Unimplemented { .. })
        ));
    }

    #[test]
    fn rejects_eh4_map_rvas_outside_the_image() {
        let mut p = process_with_image();
        put_i32(&mut p, 0x900, 0x200);
        put_bytes(&mut p, 0x200, &[0x08]);
        put_i32(&mut p, 0x201, IMAGE_SIZE as i32);
        put_i32(&mut p, 0x205, 0);

        assert!(matches!(
            parse_func_info4(&p, BASE, BASE + 0x900),
            Err(Stop::Crash(_))
        ));
    }

    fn decode_bytes(bytes: &[u8]) -> Result<u32, Stop> {
        let mut p = process_with_image();
        p.write_bytes(BASE + 0x800, bytes).unwrap();
        let mut cursor = BASE + 0x800;
        decode_eh4_uint(&p, &mut cursor, BASE + 0x800 + bytes.len() as u64)
    }

    #[test]
    fn decodes_single_byte_and_multi_byte_eh4_integers() {
        assert_eq!(decode_bytes(&[0xfe]).unwrap(), 127);
        assert_eq!(decode_bytes(&[0x05, 0x02]).unwrap(), 129);
        assert_eq!(decode_bytes(&[0x03, 0x00, 0x08]).unwrap(), 65_536);
    }

    #[test]
    fn decodes_the_largest_eh4_integer() {
        assert_eq!(
            decode_bytes(&[0x0f, 0xff, 0xff, 0xff, 0xff]).unwrap(),
            u32::MAX
        );
    }

    #[test]
    fn rejects_truncated_eh4_integers_without_advancing_the_cursor() {
        let mut p = process_with_image();
        put_bytes(&mut p, 0x800, &[0x03, 0x00]);
        let mut cursor = BASE + 0x800;

        assert!(matches!(
            decode_eh4_uint(&p, &mut cursor, BASE + 0x802),
            Err(Stop::Crash(_))
        ));
        assert_eq!(cursor, BASE + 0x800);
    }

    #[test]
    fn rejects_overlong_eh4_integer_encodings() {
        assert!(matches!(decode_bytes(&[0x01, 0x00]), Err(Stop::Crash(_))));
    }

    #[test]
    fn rejects_reserved_bits_that_would_overflow_the_eh4_five_byte_prefix() {
        assert!(matches!(
            decode_bytes(&[0x1f, 0xff, 0xff, 0xff, 0xff]),
            Err(Stop::Crash(_))
        ));
    }
}
