use anywasm_cpu::{cpu::CpuMode, decode_mode};
use std::fs;

fn u16le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn file_offset(bytes: &[u8], address: u32) -> usize {
    let pe = u32le(bytes, 0x3c) as usize;
    let sections = u16le(bytes, pe + 6) as usize;
    let optional_size = u16le(bytes, pe + 20) as usize;
    let optional = pe + 24;
    let image_base = u32le(bytes, optional + 28);
    let rva = address
        .checked_sub(image_base)
        .expect("address below image base");
    let section_table = optional + optional_size;
    for section in 0..sections {
        let entry = section_table + section * 40;
        let virtual_size = u32le(bytes, entry + 8);
        let virtual_address = u32le(bytes, entry + 12);
        let raw_size = u32le(bytes, entry + 16);
        let raw_offset = u32le(bytes, entry + 20);
        let size = virtual_size.max(raw_size);
        if rva >= virtual_address && rva < virtual_address + size {
            return (raw_offset + rva - virtual_address) as usize;
        }
    }
    panic!("address {address:#x} is not in a PE section");
}

fn print_data(bytes: &[u8], address: u32, length: usize) {
    let offset = file_offset(bytes, address);
    let data = &bytes[offset..(offset + length).min(bytes.len())];
    let text = data.split(|&byte| byte == 0).next().unwrap_or_default();
    println!(
        "data {address:#010x}: {data:02x?} {:?}",
        String::from_utf8_lossy(text)
    );
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("path to gta_sa_compact1.0.exe");
    let bytes = fs::read(path).unwrap();
    let pe = u32le(&bytes, 0x3c) as usize;
    let optional = pe + 24;
    let entrypoint = u32le(&bytes, optional + 28) + u32le(&bytes, optional + 16);
    println!("PE entrypoint: {entrypoint:#010x}");
    for address in [
        0x0088_7fe4,
        0x0088_7ff4,
        0x0085_8068,
        0x0085_806c,
        0x0085_820c,
    ] {
        print_data(&bytes, address, 32);
    }
    for (start, count) in [
        (0x0052_ddb0, 32),
        (0x0053_1150, 65),
        (0x0053_1450, 65),
        (0x0053_129e, 64),
        (0x0053_fb70, 12),
        (0x0074_8b80, 48),
        (0x0074_8ba4, 32),
        (0x0074_4d90, 40),
        (0x00cb_73e0, 8),
        (0x0082_3b00, 40),
        (0x0082_3bdb, 56),
        (0x0082_3c20, 48),
        (0x0082_3afe, 44),
        (0x0082_3cab, 32),
        (0x0082_3cbb, 32),
        (0x0082_3d83, 32),
        (0x0082_3c80, 48),
        (0x0082_3dd2, 32),
        (0x0082_3e1d, 34),
        (0x0082_3e49, 24),
        (0x0082_3e62, 40),
        (0x0082_3f93, 24),
        (0x0082_871b, 24),
        (0x0082_ad12, 24),
        (0x0082_ad12, 24),
        (0x0082_4701, 72),
        (entrypoint, 72),
    ] {
        println!("\n--- {start:#010x} ---");
        let mut rip = start;
        let mut offset = file_offset(&bytes, rip);
        for _ in 0..count {
            let slice = &bytes[offset..(offset + 15).min(bytes.len())];
            let inst = match decode_mode(slice, rip as u64, CpuMode::X86_32) {
                Ok(inst) => inst,
                Err(error) => {
                    let raw = bytes[offset..(offset + 12).min(bytes.len())]
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    println!("{rip:#010x}: {raw} {error}");
                    break;
                }
            };
            let end = offset + inst.len as usize;
            let raw = bytes[offset..end]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{rip:#010x}: {raw:<24} {:?} {:?} imm={:#x}",
                inst.op, inst.ops, inst.imm
            );
            offset = end;
            rip += inst.len as u32;
        }
    }
}
