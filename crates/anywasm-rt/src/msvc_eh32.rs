//! Initial MSVC x86 EH3 support. PE32 uses absolute-pointer metadata and a linked
//! registration node, unlike the relative-table x64 personality in `msvc_eh`.

use crate::msvc_eh::{
    match_handler, CatchHandler, CatchMatch, CatchableType, CppException, FuncInfo3, HandlerType3,
    IptoStateMapEntry3, Pmd, ThrowInfo, TryBlockMapEntry3, TypeDescriptor, UnwindMapEntry3,
};
use crate::process::{GuestCatch32, Process, Stop, TEB_ADDR};
use anywasm_cpu::cpu::{CpuMode, RAX, RBP, RSP};
use anywasm_cpu::mem::{PROT_R, PROT_X};

const MSVC_CPP_EXCEPTION: u32 = 0xe06d_7363;
const MSVC_CPP_MAGIC: u32 = 0x1993_0520;
const EXCEPTION_CONTINUE_SEARCH: u64 = 1;
const EXCEPTION_UNWINDING: u32 = 0x2;
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
const EXCEPTION_RECORD_CODE: u64 = 0;
const EXCEPTION_RECORD_FLAGS: u64 = 4;
const EXCEPTION_RECORD_PARAMETER_COUNT: u64 = 16;
const EXCEPTION_RECORD_ARGUMENTS: u64 = 20;
const CONTEXT32_EAX: u64 = 0xb0;
const CONTEXT32_EBP: u64 = 0xb4;
const CONTEXT32_EIP: u64 = 0xb8;
const CONTEXT32_ESP: u64 = 0xc4;
const CXX_TRANSFER_DATA: u64 = 32; // Unused ExceptionInformation slots after the 3 x86 C++ parameters.
const CXX_TRANSFER_MAGIC: u32 = 0x4332_5846;
const FUNC_INFO3_BYTES: u64 = 36;
const TRY_BLOCK3_BYTES: u64 = 20;
const HANDLER_TYPE32_BYTES: u64 = 16;
const UNWIND_ENTRY3_BYTES: u64 = 8;
const THROW_INFO32_BYTES: u64 = 16;
const CATCHABLE_TYPE32_BYTES: u64 = 28;
const TYPE_DESCRIPTOR32_BYTES: u64 = 8;
const MAX_EH_ENTRIES: u32 = 4096;
const MAX_TYPE_NAME_BYTES: u64 = 1024;
const MAX_CATCH_OBJECT_BYTES: u32 = 16 * 1024 * 1024;

fn malformed(message: impl Into<String>) -> Stop {
    Stop::Crash(format!("MSVC PE32 C++ EH metadata: {}", message.into()))
}

fn unsupported(name: impl Into<String>) -> Stop {
    Stop::Unimplemented {
        dll: "vcruntime".into(),
        name: name.into(),
    }
}

fn module_for_address<'a>(
    p: &'a Process,
    address: u64,
    what: &str,
) -> Result<&'a crate::process::Module, Stop> {
    p.modules
        .iter()
        .find(|module| {
            module.hle_group.is_none()
                && address >= module.base
                && address < module.base.saturating_add(module.size)
        })
        .ok_or_else(|| malformed(format!("{what} {address:#x} is outside loaded PE images")))
}

fn check_image_bytes(
    module: &crate::process::Module,
    address: u64,
    bytes: u64,
    what: &str,
) -> Result<(), Stop> {
    let end = module
        .base
        .checked_add(module.size)
        .ok_or_else(|| malformed("loaded image range overflows"))?;
    let read_end = address
        .checked_add(bytes)
        .ok_or_else(|| malformed(format!("{what} address range overflows")))?;
    if bytes == 0 || address < module.base || read_end > end {
        return Err(malformed(format!(
            "{what} range {address:#x}..{read_end:#x} is outside image {:#x}..{end:#x}",
            module.base
        )));
    }
    Ok(())
}

fn table_bytes(count: u32, stride: u64, what: &str) -> Result<u64, Stop> {
    if count > MAX_EH_ENTRIES {
        return Err(malformed(format!(
            "{what} count {count} exceeds limit {MAX_EH_ENTRIES}"
        )));
    }
    (count as u64)
        .checked_mul(stride)
        .ok_or_else(|| malformed(format!("{what} byte length overflows")))
}

fn absolute_pointer(
    p: &Process,
    module: &crate::process::Module,
    address: u64,
    bytes: u64,
    what: &str,
) -> Result<Option<u64>, Stop> {
    let target = p.rd32(address)? as u64;
    if target == 0 {
        return Ok(None);
    }
    check_image_bytes(module, target, bytes, what)?;
    Ok(Some(target))
}

fn executable_pointer(
    p: &Process,
    module: &crate::process::Module,
    address: u64,
    what: &str,
) -> Result<Option<u64>, Stop> {
    let Some(target) = absolute_pointer(p, module, address, 1, what)? else {
        return Ok(None);
    };
    if p.mem
        .prot_of(target)
        .is_none_or(|protection| protection & PROT_X == 0)
    {
        return Err(malformed(format!("{what} {target:#x} is not executable")));
    }
    Ok(Some(target))
}

fn read_type_descriptor32(
    p: &Process,
    module: &crate::process::Module,
    address: u64,
) -> Result<TypeDescriptor, Stop> {
    check_image_bytes(module, address, TYPE_DESCRIPTOR32_BYTES, "TypeDescriptor")?;
    let name_address = address + TYPE_DESCRIPTOR32_BYTES;
    let image_end = module
        .base
        .checked_add(module.size)
        .ok_or_else(|| malformed("loaded image range overflows"))?;
    let name_end = image_end.min(
        name_address
            .checked_add(MAX_TYPE_NAME_BYTES)
            .ok_or_else(|| malformed("TypeDescriptor name range overflows"))?,
    );
    if name_address >= name_end {
        return Err(malformed("TypeDescriptor name starts outside the image"));
    }
    let mut bytes = Vec::new();
    let mut cursor = name_address;
    while cursor < name_end {
        let byte = p.rd8(cursor)?;
        if byte == 0 {
            let decorated_name = String::from_utf8(bytes)
                .map_err(|_| malformed("TypeDescriptor name is not ASCII"))?;
            if decorated_name
                .bytes()
                .any(|value| !(0x20..=0x7e).contains(&value))
            {
                return Err(malformed("TypeDescriptor name has non-printable bytes"));
            }
            return Ok(TypeDescriptor {
                address,
                decorated_name,
            });
        }
        if !(0x20..=0x7e).contains(&byte) {
            return Err(malformed("TypeDescriptor name has non-printable bytes"));
        }
        bytes.push(byte);
        cursor += 1;
    }
    Err(malformed(format!(
        "TypeDescriptor name at {name_address:#x} is not terminated within {MAX_TYPE_NAME_BYTES} bytes"
    )))
}

fn parse_handler_type32(
    p: &Process,
    module: &crate::process::Module,
    address: u64,
) -> Result<HandlerType3, Stop> {
    check_image_bytes(module, address, HANDLER_TYPE32_BYTES, "x86 HandlerType")?;
    let adjectives = p.rd32(address)?;
    let type_address = p.rd32(address + 4)? as u64;
    let catch_object_displacement = p.rd32(address + 8)? as i32;
    let handler = p.rd32(address + 12)? as u64;
    let type_descriptor = if type_address == 0 {
        None
    } else {
        Some(read_type_descriptor32(p, module, type_address)?)
    };
    check_image_bytes(module, handler, 1, "x86 catch handler")?;
    if p.mem
        .prot_of(handler)
        .is_none_or(|protection| protection & PROT_X == 0)
    {
        return Err(malformed(format!(
            "x86 catch handler {handler:#x} is not executable"
        )));
    }
    Ok(HandlerType3 {
        adjectives,
        type_descriptor,
        catch_object_displacement,
        handler,
        frame_displacement: 0,
    })
}

fn parse_func_info3_32(p: &Process, function_info: u64) -> Result<(u64, FuncInfo3), Stop> {
    let module = module_for_address(p, function_info, "EH3 FuncInfo")?;
    check_image_bytes(module, function_info, FUNC_INFO3_BYTES, "x86 EH3 FuncInfo")?;
    let raw_magic = p.rd32(function_info)?;
    let magic = raw_magic & 0x1fff_ffff;
    if !matches!(magic, 0x1993_0520..=0x1993_0522) {
        return Err(unsupported(format!(
            "unsupported x86 EH3 FuncInfo magic {magic:#x}"
        )));
    }
    let max_state = p.rd32(function_info + 4)? as i32;
    if !(0..=MAX_EH_ENTRIES as i32).contains(&max_state) {
        return Err(malformed(format!(
            "x86 EH3 maxState {max_state} is invalid"
        )));
    }
    let unwind_address = p.rd32(function_info + 8)? as u64;
    let try_count = p.rd32(function_info + 12)?;
    let try_address = p.rd32(function_info + 16)? as u64;
    let ip_count = p.rd32(function_info + 20)?;
    let ip_address = p.rd32(function_info + 24)? as u64;
    let exception_specification = p.rd32(function_info + 28)? as u64;
    let eh_flags = p.rd32(function_info + 32)? as i32;
    if ip_count != 0 || ip_address != 0 {
        return Err(unsupported(
            "x86 EH3 IP-to-state maps are not part of the supported registration ABI",
        ));
    }

    let unwind_bytes = table_bytes(max_state as u32, UNWIND_ENTRY3_BYTES, "x86 unwind map")?;
    let mut unwind_map = Vec::with_capacity(max_state as usize);
    if unwind_bytes != 0 {
        check_image_bytes(module, unwind_address, unwind_bytes, "x86 unwind map")?;
        for index in 0..max_state as u64 {
            let entry = unwind_address + index * UNWIND_ENTRY3_BYTES;
            let to_state = p.rd32(entry)? as i32;
            if to_state < -1 || to_state >= max_state {
                return Err(malformed(format!(
                    "x86 unwind map state {index} targets invalid state {to_state}"
                )));
            }
            let action = executable_pointer(p, module, entry + 4, "x86 unwind funclet")?;
            unwind_map.push(UnwindMapEntry3 { to_state, action });
        }
    } else if unwind_address != 0 {
        check_image_bytes(module, unwind_address, 1, "empty x86 unwind map")?;
    }

    let try_bytes = table_bytes(try_count, TRY_BLOCK3_BYTES, "x86 try-block map")?;
    let mut try_blocks = Vec::with_capacity(try_count as usize);
    if try_bytes != 0 {
        check_image_bytes(module, try_address, try_bytes, "x86 try-block map")?;
        for index in 0..try_count as u64 {
            let entry = try_address + index * TRY_BLOCK3_BYTES;
            let try_low = p.rd32(entry)? as i32;
            let try_high = p.rd32(entry + 4)? as i32;
            let catch_high = p.rd32(entry + 8)? as i32;
            let handler_count = p.rd32(entry + 12)?;
            let handler_array = p.rd32(entry + 16)? as u64;
            if try_low < 0
                || try_high < try_low
                || try_high >= max_state
                || catch_high < try_high
                || catch_high > max_state
            {
                return Err(malformed(format!(
                    "x86 try-block {index} has invalid states {try_low}..{try_high} catch={catch_high}"
                )));
            }
            let handler_bytes =
                table_bytes(handler_count, HANDLER_TYPE32_BYTES, "x86 handler array")?;
            if handler_count == 0 {
                return Err(malformed(format!("x86 try-block {index} has no handlers")));
            }
            check_image_bytes(module, handler_array, handler_bytes, "x86 handler array")?;
            let mut handlers = Vec::with_capacity(handler_count as usize);
            for handler_index in 0..handler_count as u64 {
                handlers.push(parse_handler_type32(
                    p,
                    module,
                    handler_array + handler_index * HANDLER_TYPE32_BYTES,
                )?);
            }
            try_blocks.push(TryBlockMapEntry3 {
                try_low,
                try_high,
                catch_high,
                handlers,
            });
        }
    } else if try_address != 0 {
        check_image_bytes(module, try_address, 1, "empty x86 try-block map")?;
    }

    let exception_specification = if exception_specification == 0 {
        None
    } else {
        check_image_bytes(
            module,
            exception_specification,
            8,
            "x86 exception specification",
        )?;
        let count = p.rd32(exception_specification)? as i32;
        if count < 0 || count as u32 > MAX_EH_ENTRIES {
            return Err(malformed(format!(
                "x86 exception specification count {count} is invalid"
            )));
        }
        let types = p.rd32(exception_specification + 4)? as u64;
        let bytes = table_bytes(count as u32, HANDLER_TYPE32_BYTES, "x86 exception types")?;
        if bytes != 0 {
            check_image_bytes(module, types, bytes, "x86 exception types")?;
        }
        Some(exception_specification)
    };

    Ok((
        module.base,
        FuncInfo3 {
            magic,
            max_state,
            unwind_map,
            try_blocks,
            ip_to_state: Vec::<IptoStateMapEntry3>::new(),
            unwind_help: None,
            exception_specification,
            eh_flags,
        },
    ))
}

fn parse_throw_info32(p: &Process, throw_info_address: u64) -> Result<CppException, Stop> {
    let module = module_for_address(p, throw_info_address, "x86 ThrowInfo")?;
    check_image_bytes(
        module,
        throw_info_address,
        THROW_INFO32_BYTES,
        "x86 ThrowInfo",
    )?;
    let attributes = p.rd32(throw_info_address)?;
    let unwind_function =
        executable_pointer(p, module, throw_info_address + 4, "throw destructor")?;
    let _forward_compatibility = executable_pointer(
        p,
        module,
        throw_info_address + 8,
        "throw forward-compatibility handler",
    )?;
    let catchable_array =
        absolute_pointer(p, module, throw_info_address + 12, 4, "CatchableTypeArray")?
            .ok_or_else(|| malformed("x86 ThrowInfo has a null CatchableTypeArray"))?;
    let count = p.rd32(catchable_array)?;
    if count == 0 {
        return Err(malformed("x86 CatchableTypeArray is empty"));
    }
    let pointer_bytes = table_bytes(count, 4, "x86 catchable-type pointer array")?;
    check_image_bytes(
        module,
        catchable_array + 4,
        pointer_bytes,
        "x86 catchable-type pointer array",
    )?;
    let mut catchable_types = Vec::with_capacity(count as usize);
    for index in 0..count as u64 {
        let type_address = p.rd32(catchable_array + 4 + index * 4)? as u64;
        check_image_bytes(
            module,
            type_address,
            CATCHABLE_TYPE32_BYTES,
            "x86 CatchableType",
        )?;
        let properties = p.rd32(type_address)?;
        let descriptor_address = p.rd32(type_address + 4)? as u64;
        let type_descriptor = read_type_descriptor32(p, module, descriptor_address)?;
        let this_displacement = Pmd {
            member_displacement: p.rd32(type_address + 8)? as i32,
            vbtable_displacement: p.rd32(type_address + 12)? as i32,
            vbase_displacement: p.rd32(type_address + 16)? as i32,
        };
        let size_or_offset = p.rd32(type_address + 20)?;
        if size_or_offset == 0 || size_or_offset > MAX_CATCH_OBJECT_BYTES {
            return Err(malformed(format!(
                "x86 CatchableType size {size_or_offset} is invalid"
            )));
        }
        let copy_function = executable_pointer(p, module, type_address + 24, "copy function")?;
        catchable_types.push(CatchableType {
            properties,
            type_descriptor,
            this_displacement,
            size_or_offset,
            copy_function,
        });
    }
    Ok(CppException {
        object_address: 0,
        throw_info: ThrowInfo {
            attributes,
            unwind_function,
            forward_compatibility: _forward_compatibility,
            catchable_types,
        },
    })
}

fn exception_from_record32(p: &Process, exception_record: u64) -> Result<CppException, Stop> {
    if p.rd32(exception_record + EXCEPTION_RECORD_CODE)? != MSVC_CPP_EXCEPTION {
        return Err(malformed("frame handler received a non-C++ exception"));
    }
    let parameter_count = p.rd32(exception_record + EXCEPTION_RECORD_PARAMETER_COUNT)?;
    if parameter_count != 3 {
        return Err(malformed(format!(
            "x86 C++ exception has {parameter_count} parameters; expected 3"
        )));
    }
    if p.rd32(exception_record + EXCEPTION_RECORD_ARGUMENTS)? != MSVC_CPP_MAGIC {
        return Err(malformed("x86 C++ exception has an unknown magic value"));
    }
    let object_address = p.rd32(exception_record + EXCEPTION_RECORD_ARGUMENTS + 4)? as u64;
    let throw_info_address = p.rd32(exception_record + EXCEPTION_RECORD_ARGUMENTS + 8)? as u64;
    if object_address == 0 || p.mem.prot_of(object_address).is_none() {
        return Err(malformed(format!(
            "x86 thrown object address {object_address:#x} is not mapped"
        )));
    }
    let mut exception = parse_throw_info32(p, throw_info_address)?;
    exception.object_address = object_address;
    Ok(exception)
}

fn active_try_indices(info: &FuncInfo3, state: i32) -> Vec<usize> {
    let mut indices = info
        .try_blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.try_low <= state && state <= block.try_high).then_some(index)
        })
        .collect::<Vec<_>>();
    indices.sort_by_key(|&index| {
        let block = &info.try_blocks[index];
        (std::cmp::Reverse(block.try_low), block.try_high)
    });
    indices
}

#[derive(Clone, Copy)]
struct CatchTransfer32 {
    target_frame: u64,
    target_ip: u64,
    target_state: i32,
    catch_state: i32,
    try_index: usize,
    handler_index: usize,
}

fn read_catch_transfer32(p: &Process, exception_record: u64) -> Result<CatchTransfer32, Stop> {
    let data = exception_record + CXX_TRANSFER_DATA;
    if p.rd32(data)? != CXX_TRANSFER_MAGIC {
        return Err(malformed(
            "x86 target unwind has no C++ catch transfer record",
        ));
    }
    Ok(CatchTransfer32 {
        target_frame: p.rd32(data + 4)? as u64,
        target_ip: p.rd32(data + 8)? as u64,
        target_state: p.rd32(data + 12)? as i32,
        catch_state: p.rd32(data + 16)? as i32,
        try_index: p.rd32(data + 20)? as usize,
        handler_index: p.rd32(data + 24)? as usize,
    })
}

/// Called by the compiler-emitted per-function handler thunk. The thunk places the absolute
/// FuncInfo pointer in EAX and jumps to the imported `__CxxFrameHandler3` entry point.
pub(crate) fn frame_handler3(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(unsupported("x86 EH3 handler used by a non-PE32 image"));
    }
    let exception_record = p.arg(0) as u32 as u64;
    let registration = p.arg(1) as u32 as u64;
    let context_record = p.arg(2) as u32 as u64;
    let flags = p.rd32(exception_record + EXCEPTION_RECORD_FLAGS)?;
    let code = p.rd32(exception_record + EXCEPTION_RECORD_CODE)?;
    let function_info = p.cpu.gpr[RAX] as u32 as u64;
    let (_, info) = parse_func_info3_32(p, function_info)?;
    let state = p.rd32(registration + 8)? as i32;
    if state < -1 || state > info.max_state {
        return Err(malformed(format!(
            "x86 registration state {state} is invalid"
        )));
    }
    let frame_base = registration
        .checked_add(12)
        .ok_or_else(|| malformed("x86 registration frame address overflows"))?;

    if flags & EXCEPTION_UNWINDING != 0 {
        if code == MSVC_CPP_EXCEPTION && flags & EXCEPTION_TARGET_UNWIND != 0 {
            let transfer = read_catch_transfer32(p, exception_record)?;
            if transfer.target_frame != registration {
                return Err(malformed(format!(
                    "x86 target unwind frame {registration:#x} does not match selected frame {:#x}",
                    transfer.target_frame
                )));
            }
            let block = info
                .try_blocks
                .get(transfer.try_index)
                .ok_or_else(|| malformed("x86 selected try-block index is out of range"))?;
            let handler = block
                .handlers
                .get(transfer.handler_index)
                .ok_or_else(|| malformed("x86 selected handler index is out of range"))?;
            if handler.handler != transfer.target_ip
                || block.try_low.checked_sub(1) != Some(transfer.target_state)
                || block.catch_high != transfer.catch_state
            {
                return Err(malformed("x86 catch transfer does not match EH3 metadata"));
            }
            let exception = exception_from_record32(p, exception_record)?;
            let generic_handler = CatchHandler {
                adjectives: handler.adjectives,
                decorated_type_name: handler
                    .type_descriptor
                    .as_ref()
                    .map(|descriptor| descriptor.decorated_name.clone()),
                catch_object_displacement: handler.catch_object_displacement as i64,
            };
            let matched = match_handler(p, &exception, &generic_handler)?
                .ok_or_else(|| malformed("selected x86 catch no longer matches the thrown type"))?;
            unwind_state_map(
                p,
                &info,
                registration,
                state,
                transfer.target_state,
                frame_base,
            )?;
            build_catch_object32(p, frame_base, &exception, &matched)?;
            p.wr32(registration + 8, transfer.catch_state as u32)?;

            let stack_slot = registration
                .checked_sub(4)
                .ok_or_else(|| malformed("x86 registration stack slot underflows"))?;
            let stack_base = p.rd32(TEB_ADDR + 4)? as u64;
            let stack_limit = p.rd32(TEB_ADDR + 8)? as u64;
            if stack_slot < stack_limit || stack_slot + 4 > stack_base || frame_base > stack_base {
                return Err(malformed(
                    "x86 catch registration lies outside TEB stack bounds",
                ));
            }
            let stack_pointer = p.rd32(stack_slot)? as u64;
            if stack_pointer < stack_limit
                || stack_pointer >= frame_base
                || stack_pointer >= stack_base
            {
                return Err(malformed(format!(
                    "x86 catch stack pointer {stack_pointer:#x} is outside TEB bounds"
                )));
            }
            p.wr32(context_record + CONTEXT32_EBP, frame_base as u32)?;
            p.wr32(context_record + CONTEXT32_ESP, stack_pointer as u32)?;
            p.wr32(context_record + CONTEXT32_EIP, transfer.target_ip as u32)?;
            p.wr32(context_record + CONTEXT32_EAX, 0)?;
        } else {
            unwind_state_map(p, &info, registration, state, -1, frame_base)?;
            p.wr32(registration + 8, u32::MAX)?;
        }
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    }
    if code != MSVC_CPP_EXCEPTION {
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    }

    let exception = exception_from_record32(p, exception_record)?;
    let mut selected = None;
    for try_index in active_try_indices(&info, state) {
        for (handler_index, handler) in info.try_blocks[try_index].handlers.iter().enumerate() {
            let generic_handler = CatchHandler {
                adjectives: handler.adjectives,
                decorated_type_name: handler
                    .type_descriptor
                    .as_ref()
                    .map(|descriptor| descriptor.decorated_name.clone()),
                catch_object_displacement: handler.catch_object_displacement as i64,
            };
            if match_handler(p, &exception, &generic_handler)?.is_some() {
                selected = Some((try_index, handler_index));
                break;
            }
        }
        if selected.is_some() {
            break;
        }
    }
    let Some((try_index, handler_index)) = selected else {
        return Ok(EXCEPTION_CONTINUE_SEARCH);
    };
    let block = &info.try_blocks[try_index];
    Err(Stop::GuestCatch32(GuestCatch32 {
        target_frame: registration,
        target_ip: block.handlers[handler_index].handler,
        target_state: block.try_low - 1,
        catch_state: block.catch_high,
        try_index,
        handler_index,
    }))
}

fn unwind_state_map(
    p: &mut Process,
    info: &FuncInfo3,
    registration: u64,
    mut state: i32,
    target_state: i32,
    frame_base: u64,
) -> Result<(), Stop> {
    if target_state < -1 || state < target_state {
        return Err(malformed(format!(
            "x86 unwind range {state} -> {target_state} is invalid"
        )));
    }
    while state > target_state {
        let entry = info
            .unwind_map
            .get(state as usize)
            .ok_or_else(|| malformed(format!("x86 unwind state {state} is out of range")))?;
        if entry.to_state >= state {
            return Err(malformed(format!(
                "x86 unwind map does not make progress from state {state}"
            )));
        }
        p.wr32(registration + 8, entry.to_state as u32)?;
        if let Some(action) = entry.action {
            let previous_ebp = p.cpu.gpr[RBP];
            p.cpu.gpr[RBP] = frame_base;
            let result = p.call_guest(action, &[]);
            p.cpu.gpr[RBP] = previous_ebp;
            if let Err(stop) = result {
                return Err(match stop {
                    Stop::GuestUnwind(_) | Stop::GuestCatch32(_) => {
                        unsupported("a guest exception was raised from an x86 C++ unwind funclet")
                    }
                    other => other,
                });
            }
        }
        state = entry.to_state;
    }
    Ok(())
}

fn build_catch_object32(
    p: &mut Process,
    frame_base: u64,
    exception: &CppException,
    matched: &CatchMatch,
) -> Result<u64, Stop> {
    if matched.is_catch_all {
        return Ok(0);
    }
    let source_size = matched.object_size;
    let destination_size = if matched.by_reference { 4 } else { source_size };
    if source_size == 0 || source_size > MAX_CATCH_OBJECT_BYTES {
        return Err(malformed(format!(
            "x86 catch object size {source_size} is invalid"
        )));
    }
    let destination = frame_base
        .checked_add_signed(matched.catch_object_displacement)
        .ok_or_else(|| malformed("x86 catch object address overflows"))?;
    let stack_base = p.rd32(TEB_ADDR + 4)? as u64;
    let stack_limit = p.rd32(TEB_ADDR + 8)? as u64;
    let destination_end = destination
        .checked_add(destination_size as u64)
        .ok_or_else(|| malformed("x86 catch object range overflows"))?;
    if destination < stack_limit || destination_end > stack_base {
        return Err(malformed(format!(
            "x86 catch object {destination:#x}..{destination_end:#x} is outside the TEB stack"
        )));
    }
    let source_end = matched
        .adjusted_object_address
        .checked_add(source_size as u64)
        .ok_or_else(|| malformed("x86 thrown object range overflows"))?;
    if matched.adjusted_object_address < exception.object_address
        || p.mem
            .prot_of(matched.adjusted_object_address)
            .is_none_or(|prot| prot & PROT_R == 0)
        || p.mem
            .prot_of(source_end.saturating_sub(1))
            .is_none_or(|prot| prot & PROT_R == 0)
    {
        return Err(malformed("x86 thrown object range is not readable"));
    }
    if matched.by_reference {
        p.wr32(destination, matched.adjusted_object_address as u32)?;
    } else if matched.copy_function.is_some() {
        return Err(unsupported(
            "x86 catch copy-constructor closures are not implemented",
        ));
    } else {
        let bytes = p.read_bytes(matched.adjusted_object_address, source_size as usize)?;
        p.write_bytes(destination, &bytes)?;
    }
    Ok(destination)
}

pub(crate) fn cxx_throw_exception(p: &mut Process) -> Result<u64, Stop> {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(unsupported(
            "PE32 _CxxThrowException called outside x86 mode",
        ));
    }
    let object_address = p.arg(0) as u32 as u64;
    let throw_info = p.arg(1) as u32 as u64;
    let mut exception = parse_throw_info32(p, throw_info)?;
    exception.object_address = object_address;
    let primary_size = exception
        .throw_info
        .catchable_types
        .first()
        .map(|catchable| catchable.size_or_offset)
        .ok_or_else(|| malformed("x86 ThrowInfo has no primary catchable type"))?;
    let object_end = object_address
        .checked_add(primary_size as u64)
        .ok_or_else(|| malformed("x86 thrown object range overflows"))?;
    if object_end <= object_address
        || p.mem
            .prot_of(object_address)
            .is_none_or(|prot| prot & PROT_R == 0)
        || p.mem
            .prot_of(object_end - 1)
            .is_none_or(|prot| prot & PROT_R == 0)
    {
        return Err(malformed("x86 thrown object range is not readable"));
    }
    let caller = p.rd32(p.cpu.gpr[RSP])? as u64;
    crate::exception32::dispatch_cxx_exception(
        p,
        MSVC_CPP_EXCEPTION,
        1,
        &[MSVC_CPP_MAGIC as u64, object_address, throw_info],
        caller,
    )
}
