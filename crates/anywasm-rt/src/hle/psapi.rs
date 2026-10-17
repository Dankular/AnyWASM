//! Process status APIs backed by the runtime's loaded-module table.

use crate::process::{Handle, HleFn, Process, Stop};
use anywasm_cpu::CpuMode;

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_DATA: u32 = 13;
const CURRENT_PROCESS_ID: u32 = 0x1000;

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "GetModuleInformation" => Some(get_module_information),
        _ => None,
    }
}

fn current_process_handle(p: &Process, handle: u64) -> bool {
    handle == u64::MAX
        || handle == u32::MAX as u64
        || matches!(p.handles.get(&handle), Some(Handle::Process { id, .. }) if *id == CURRENT_PROCESS_ID)
}

fn get_module_information(p: &mut Process) -> Result<u64, Stop> {
    let process_handle = p.arg(0);
    let module_handle = p.arg(1);
    let output = p.arg(2);
    let output_size = p.arg(3) as u32;

    if !current_process_handle(p, process_handle) {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    }
    if output == 0 {
        p.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let Some((module_base, module_size, module_entry)) = p
        .modules
        .iter()
        .find(|module| module.base == module_handle)
        .map(|module| (module.base, module.size, module.entry))
    else {
        p.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    let Some(image_size) = u32::try_from(module_size).ok() else {
        p.set_last_error(ERROR_INVALID_DATA);
        return Ok(0);
    };

    match p.cpu.mode {
        CpuMode::X86_32 => {
            if output_size < 12 {
                p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
                return Ok(0);
            }
            let Some(base) = u32::try_from(module_base).ok() else {
                p.set_last_error(ERROR_INVALID_DATA);
                return Ok(0);
            };
            let Some(entry) = u32::try_from(module_entry).ok() else {
                p.set_last_error(ERROR_INVALID_DATA);
                return Ok(0);
            };
            p.wr32(output, base)?;
            p.wr32(output + 4, image_size)?;
            p.wr32(output + 8, entry)?;
        }
        CpuMode::X86_64 => {
            if output_size < 24 {
                p.set_last_error(ERROR_INSUFFICIENT_BUFFER);
                return Ok(0);
            }
            p.wr64(output, module_base)?;
            p.wr32(output + 8, image_size)?;
            p.wr32(output + 12, 0)?;
            p.wr64(output + 16, module_entry)?;
        }
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{process::Module, process::TEB_ADDR, vfs::Vfs};
    use anywasm_cpu::{cpu::RSP, mem::PROT_RW, CpuMode};
    use std::collections::HashMap;

    fn set_args(p: &mut Process, args: &[u64]) {
        let stack = p.alloc_virtual(0x1000, PROT_RW);
        p.cpu.gpr[RSP] = stack;
        p.wr32(stack, 0xfeed_face).unwrap();
        for (index, arg) in args.iter().copied().enumerate() {
            p.wr32(stack + 4 + index as u64 * 4, arg as u32).unwrap();
        }
    }

    fn x86_process() -> Process {
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = CpuMode::X86_32;
        p.vmem_next = 0x0010_0000;
        assert!(p.mem.map(TEB_ADDR, 0x1000, PROT_RW));
        p
    }

    fn test_module() -> Module {
        Module {
            name: "game.exe".into(),
            path: "C:\\game.exe".into(),
            base: 0x400000,
            size: 0x12000,
            entry: 0x401000,
            is_dll: false,
            tls_callbacks: Vec::new(),
            exports: HashMap::new(),
            ordinals: HashMap::new(),
            exception_table_rva: 0,
            runtime_functions: Vec::new(),
            resource_rva: 0,
            resource_size: 0,
            hle_group: None,
        }
    }

    #[test]
    fn reports_real_x86_loaded_module_information() {
        let mut p = x86_process();
        p.modules.push(test_module());
        let output = p.alloc_virtual(0x1000, PROT_RW);
        set_args(&mut p, &[u32::MAX as u64, 0x400000, output, 12]);

        assert_eq!(lookup("GetModuleInformation").unwrap()(&mut p), Ok(1));
        assert_eq!(p.rd32(output).unwrap(), 0x400000);
        assert_eq!(p.rd32(output + 4).unwrap(), 0x12000);
        assert_eq!(p.rd32(output + 8).unwrap(), 0x401000);
    }

    #[test]
    fn rejects_short_output_and_invalid_module_without_claiming_success() {
        let mut p = x86_process();
        p.modules.push(test_module());
        let output = p.alloc_virtual(0x1000, PROT_RW);

        set_args(&mut p, &[u32::MAX as u64, 0x400000, output, 11]);
        assert_eq!(lookup("GetModuleInformation").unwrap()(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INSUFFICIENT_BUFFER);

        set_args(&mut p, &[u32::MAX as u64, 0xdead0000, output, 12]);
        assert_eq!(lookup("GetModuleInformation").unwrap()(&mut p), Ok(0));
        assert_eq!(p.rd32(TEB_ADDR + 0x34).unwrap(), ERROR_INVALID_HANDLE);
    }
}
