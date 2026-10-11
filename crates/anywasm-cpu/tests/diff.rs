//! Differential test: run every instruction in `corpus.txt` on the *host CPU* and in the
//! interpreter with identical random state, then compare GPRs, flags, XMM registers and memory.
//!
//! Only meaningful on x86-64 Linux. Flags the architecture leaves undefined for an instruction are
//! masked out (table in `flag_mask`). Set `DIFF_FILTER=<substring>` to run a subset and
//! `DIFF_TRIALS=<n>` to change the trial count.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use anywasm_cpu::cpu::*;
use anywasm_cpu::mem::{PROT_RW, PROT_RWX};
use anywasm_cpu::{step, Cpu, Memory};
use std::collections::BTreeMap;
#[path = "support/diff_filter.rs"]
mod diff_filter;
use diff_filter::{flag_mask, mnemonic, skip};

const DATA: u64 = 0x1000_0000;
const CODE: u64 = 0x1001_0000;
const STACK: u64 = 0x1002_0000;
const STACK_SIZE: u64 = 0x1000;
const STACK_TOP: u64 = STACK + STACK_SIZE / 2;
const DATA_SPAN: usize = 0x1000;
const SCR: u64 = DATA + 0x400;

#[repr(C)]
#[derive(Clone)]
struct HState {
    gpr: [u64; 16],
    rflags: u64,
    code: u64,
    xmm: [[u8; 16]; 16],
}

std::arch::global_asm!(
    ".globl anywasm_run",
    "anywasm_run:",
    "push rbx", "push rbp", "push r12", "push r13", "push r14", "push r15",
    "push rdi",
    "push qword ptr [rdi+136]",
    "movups xmm0,  [rdi+144]", "movups xmm1,  [rdi+160]", "movups xmm2,  [rdi+176]", "movups xmm3,  [rdi+192]",
    "movups xmm4,  [rdi+208]", "movups xmm5,  [rdi+224]", "movups xmm6,  [rdi+240]", "movups xmm7,  [rdi+256]",
    "movups xmm8,  [rdi+272]", "movups xmm9,  [rdi+288]", "movups xmm10, [rdi+304]", "movups xmm11, [rdi+320]",
    "movups xmm12, [rdi+336]", "movups xmm13, [rdi+352]", "movups xmm14, [rdi+368]", "movups xmm15, [rdi+384]",
    "push qword ptr [rdi+128]",
    "popfq",
    "mov rax,[rdi+0]", "mov rcx,[rdi+8]", "mov rdx,[rdi+16]", "mov rbx,[rdi+24]",
    "mov rbp,[rdi+40]", "mov rsi,[rdi+48]",
    "mov r8,[rdi+64]", "mov r9,[rdi+72]", "mov r10,[rdi+80]", "mov r11,[rdi+88]",
    "mov r12,[rdi+96]", "mov r13,[rdi+104]", "mov r14,[rdi+112]", "mov r15,[rdi+120]",
    "mov rdi,[rdi+56]",
    "call qword ptr [rsp]",
    "xchg rdi,[rsp+8]",
    "mov [rdi+0],rax", "mov [rdi+8],rcx", "mov [rdi+16],rdx", "mov [rdi+24],rbx",
    "mov [rdi+40],rbp", "mov [rdi+48],rsi",
    "mov [rdi+64],r8", "mov [rdi+72],r9", "mov [rdi+80],r10", "mov [rdi+88],r11",
    "mov [rdi+96],r12", "mov [rdi+104],r13", "mov [rdi+112],r14", "mov [rdi+120],r15",
    "pushfq",
    "pop qword ptr [rdi+128]",
    "movups [rdi+144], xmm0", "movups [rdi+160], xmm1", "movups [rdi+176], xmm2", "movups [rdi+192], xmm3",
    "movups [rdi+208], xmm4", "movups [rdi+224], xmm5", "movups [rdi+240], xmm6", "movups [rdi+256], xmm7",
    "movups [rdi+272], xmm8", "movups [rdi+288], xmm9", "movups [rdi+304], xmm10", "movups [rdi+320], xmm11",
    "movups [rdi+336], xmm12", "movups [rdi+352], xmm13", "movups [rdi+368], xmm14", "movups [rdi+384], xmm15",
    "mov rax,[rsp+8]",
    "mov [rdi+56],rax",
    "cld",
    "add rsp,16",
    "pop r15", "pop r14", "pop r13", "pop r12", "pop rbp", "pop rbx",
    "ret",
);

extern "C" {
    fn anywasm_run(st: *mut HState);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const INTERESTING: [u64; 14] = [
    0, 1, u64::MAX, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x7fff_ffff_ffff_ffff,
    0x8000_0000_0000_0000,
];

fn rand_u64(r: &mut Rng) -> u64 {
    match r.below(4) {
        0 => INTERESTING[r.below(INTERESTING.len() as u64) as usize],
        1 => r.next() & 0xff,
        _ => r.next(),
    }
}

fn nice_f32(r: &mut Rng) -> u32 {
    match r.below(8) {
        0 => 0,
        1 => 0x8000_0000,
        2 => 0x7f80_0000,
        3 => 0xff80_0000,
        4 => 0x7fc0_0000,
        5 => (1.0f32).to_bits(),
        _ => (((r.next() % 20000) as f32 - 10000.0) / 7.0).to_bits(),
    }
}
fn nice_f64(r: &mut Rng) -> u64 {
    match r.below(8) {
        0 => 0,
        1 => 0x8000_0000_0000_0000,
        2 => 0x7ff0_0000_0000_0000,
        3 => 0xfff0_0000_0000_0000,
        4 => 0x7ff8_0000_0000_0000,
        5 => (-2.5f64).to_bits(),
        _ => (((r.next() % 2_000_000) as f64 - 1_000_000.0) / 13.0).to_bits(),
    }
}

fn rand_xmm(r: &mut Rng) -> [u8; 16] {
    let v: u128 = match r.below(5) {
        0 => (r.next() as u128) << 64 | r.next() as u128,
        1 => {
            let l = [nice_f32(r), nice_f32(r), nice_f32(r), nice_f32(r)];
            l[0] as u128 | (l[1] as u128) << 32 | (l[2] as u128) << 64 | (l[3] as u128) << 96
        }
        2 => nice_f64(r) as u128 | (nice_f64(r) as u128) << 64,
        3 => {
            // small integers per byte: exercises saturation edges
            let mut x = 0u128;
            for i in 0..16 {
                let b = [0u8, 1, 0x7f, 0x80, 0xff, 2, 0xfe][r.below(7) as usize];
                x |= (b as u128) << (8 * i);
            }
            x
        }
        _ => {
            let w = [rand_u64(r) as u16, rand_u64(r) as u16, 0x8000, 0x7fff, 0xffff, 0, 1, rand_u64(r) as u16];
            let mut x = 0u128;
            for (i, v) in w.iter().enumerate() {
                x |= (*v as u128) << (16 * i);
            }
            x
        }
    };
    v.to_le_bytes()
}

fn op_size_bits(asm: &str) -> u32 {
    let ops = asm.splitn(2, ' ').nth(1).unwrap_or("");
    let first = ops.split(',').next().unwrap().trim();
    if first.starts_with("byte") {
        8
    } else if first.starts_with("word") {
        16
    } else if first.starts_with("dword") {
        32
    } else if first.starts_with("qword") {
        64
    } else if matches!(first, "al" | "cl" | "dl" | "bl" | "ah" | "ch" | "dh" | "bh" | "bpl") || first.ends_with('b') && first.starts_with('r') {
        8
    } else if matches!(first, "ax" | "cx" | "dx" | "bx" | "bp") || first.ends_with('w') && first.starts_with('r') {
        16
    } else if first.starts_with('e') || first.ends_with('d') && first.starts_with('r') {
        32
    } else {
        64
    }
}

fn is_string_op(asm: &str) -> bool {
    matches!(mnemonic(asm), "movsb" | "movsw" | "movsq" | "stosb" | "stosw" | "stosd" | "stosq" | "lodsb" | "lodsw" | "lodsd" | "lodsq"
        | "scasb" | "scasw" | "scasd" | "scasq" | "cmpsb" | "cmpsw" | "cmpsq")
        || (matches!(mnemonic(asm), "movsd" | "cmpsd") && asm.contains("ptr [rdi]"))
}

struct Case {
    st: HState,
    data: Vec<u8>,
}

fn make_case(asm: &str, r: &mut Rng) -> Case {
    let mut st = HState { gpr: [0; 16], rflags: 0x202, code: CODE, xmm: [[0; 16]; 16] };
    for g in st.gpr.iter_mut() {
        *g = rand_u64(r);
    }
    st.rflags = 0x202 | (r.next() & ARITH_FLAGS);
    for x in st.xmm.iter_mut() {
        *x = rand_xmm(r);
    }
    let mut data = vec![0u8; DATA_SPAN];
    let mut i = 0;
    while i < DATA_SPAN {
        let bytes = rand_xmm(r);
        data[i..i + 16].copy_from_slice(&bytes);
        i += 16;
    }
    if is_x87(asm) {
        fill_x87_data(&mut data, r);
    }
    let m = mnemonic(asm);
    let mem = asm.contains('[');
    if mem {
        st.gpr[RSI] = SCR;
        st.gpr[RDI] = r.below(4);
    }
    if asm.split(';').any(|instruction| {
        let instruction = instruction.trim();
        !instruction.is_empty() && matches!(mnemonic(instruction), "call" | "ret")
    }) {
        st.gpr[RSP] = STACK_TOP;
    }
    if is_string_op(asm) || asm.starts_with("rep") {
        st.gpr[RSI] = SCR + 0x100;
        st.gpr[RDI] = SCR + 0x500;
        st.gpr[RCX] = r.below(20);
        if r.below(3) == 0 {
            st.rflags |= DF;
        }
        // make rep scas/cmps stop sometimes: plant equal bytes
        if r.below(2) == 0 {
            let src = (SCR + 0x100 - DATA) as usize;
            let dst = (SCR + 0x500 - DATA) as usize;
            for k in 0..64 {
                data[dst - 32 + k] = data[src - 32 + k];
            }
        }
    }
    if matches!(m, "div" | "idiv") {
        let bits = op_size_bits(asm);
        let d = r.below(100) + 2;
        let q = r.below(100);
        let rem = r.below(d);
        let mut dividend = (d * q + rem) as i128;
        let mut div = d as i128;
        if m == "idiv" {
            if r.below(2) == 0 {
                dividend = -dividend;
            }
            if r.below(2) == 0 {
                div = -div;
            }
        }
        let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        // divisor in rcx and in memory
        st.gpr[RCX] = div as i64 as u64;
        let off = (SCR + 0x10 - DATA) as usize;
        data[off..off + 8].copy_from_slice(&(div as i64 as u64).to_le_bytes());
        if bits == 8 {
            st.gpr[RAX] = (st.gpr[RAX] & !0xffff) | (dividend as i64 as u64 & 0xffff);
        } else {
            let wide = dividend as i128;
            st.gpr[RAX] = (wide as u64) & mask | if bits < 64 { st.gpr[RAX] & !mask } else { 0 };
            let hi = ((wide >> bits) as u64) & mask;
            st.gpr[RDX] = hi | if bits < 64 { st.gpr[RDX] & !mask } else { 0 };
        }
        if bits < 64 && bits != 8 {
            st.gpr[RAX] &= mask;
            st.gpr[RDX] &= mask;
        }
        st.gpr[RAX] = if bits == 8 { st.gpr[RAX] & 0xffff } else { st.gpr[RAX] };
    }
    if matches!(m, "shld" | "shrd") && op_size_bits(asm) == 16 && asm.ends_with("cl") {
        // 16-bit double shifts with count > 16 are undefined and vendor-specific (this passed on
        // an Intel Xeon and failed on a CI runner); only test the defined range 0..=16.
        st.gpr[RCX] = (st.gpr[RCX] & 0xff) % 17;
    }
    if matches!(m, "bt" | "bts" | "btr" | "btc") && mem {
        st.gpr[RCX] &= 0xff;
    }
    if m == "cmpxchg16b" {
        // fine: memory is 16-byte aligned by construction
    }
    Case { st, data }
}

fn setup_host() {
    unsafe {
        for (addr, len, prot) in [(DATA, 0x10000usize, libc::PROT_READ | libc::PROT_WRITE), (CODE, 0x1000, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC)] {
            let p = libc::mmap(
                addr as *mut libc::c_void,
                len,
                prot,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                -1,
                0,
            );
            assert_eq!(p as u64, addr, "could not map host test region at {addr:#x}");
        }
    }
}

fn run_host(code: &[u8], c: &Case) -> (HState, Vec<u8>) {
    unsafe {
        let cp = CODE as *mut u8;
        std::ptr::copy_nonoverlapping(code.as_ptr(), cp, code.len());
        *cp.add(code.len()) = 0xC3;
        std::ptr::copy_nonoverlapping(c.data.as_ptr(), DATA as *mut u8, DATA_SPAN);
        let mut st = c.st.clone();
        anywasm_run(&mut st);
        let mut out = vec![0u8; DATA_SPAN];
        std::ptr::copy_nonoverlapping(DATA as *const u8, out.as_mut_ptr(), DATA_SPAN);
        (st, out)
    }
}

fn run_emu(code: &[u8], c: &Case) -> Result<(HState, Vec<u8>), String> {
    let mut mem = Memory::new();
    mem.map(DATA, 0x10000, PROT_RW);
    mem.map(CODE, 0x1000, PROT_RWX);
    mem.map(STACK, STACK_SIZE, PROT_RW);
    mem.write_force(CODE, code).unwrap();
    mem.write_force(DATA, &c.data).unwrap();
    let mut cpu = Cpu::new();
    cpu.gpr = c.st.gpr;
    cpu.rflags = c.st.rflags;
    for i in 0..16 {
        cpu.xmm[i] = u128::from_le_bytes(c.st.xmm[i]);
    }
    cpu.rip = CODE;
    let end = CODE + code.len() as u64;
    let mut steps = 0;
    while cpu.rip != end {
        step(&mut cpu, &mut mem).map_err(|e| format!("{e:?} at +{:#x}", cpu.rip - CODE))?;
        steps += 1;
        if steps > 512 {
            return Err("runaway".into());
        }
    }
    if cpu.rip != end {
        return Err(format!("rip {:#x} != expected {:#x}", cpu.rip, CODE + code.len() as u64));
    }
    let mut st = c.st.clone();
    st.gpr = cpu.gpr;
    st.rflags = cpu.rflags;
    for i in 0..16 {
        st.xmm[i] = cpu.xmm[i].to_le_bytes();
    }
    let out = mem.read_vec(DATA, DATA_SPAN).unwrap();
    Ok((st, out))
}

fn lanes_match_modulo_nan(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let f32_ok = (0..4).all(|i| {
        let (x, y) = (u32::from_le_bytes(a[4 * i..4 * i + 4].try_into().unwrap()), u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap()));
        x == y || (f32::from_bits(x).is_nan() && f32::from_bits(y).is_nan())
    });
    let f64_ok = (0..2).all(|i| {
        let (x, y) = (u64::from_le_bytes(a[8 * i..8 * i + 8].try_into().unwrap()), u64::from_le_bytes(b[8 * i..8 * i + 8].try_into().unwrap()));
        x == y || (f64::from_bits(x).is_nan() && f64::from_bits(y).is_nan())
    });
    f32_ok || f64_ok
}

fn compare(asm: &str, c: &Case, h: &(HState, Vec<u8>), e: &(HState, Vec<u8>)) -> Option<String> {
    for i in 0..16 {
        if i == RSP {
            continue;
        }
        let m = if is_x87(asm) && i == RAX { !0x80ffu64 } else { u64::MAX }; // x87 status: ignore exception flags/ES/B
        if (h.0.gpr[i] ^ e.0.gpr[i]) & m != 0 {
            return Some(format!("gpr[{i}] host={:#x} emu={:#x}", h.0.gpr[i], e.0.gpr[i]));
        }
    }
    let mask = flag_mask(asm, &c.st.gpr);
    if (h.0.rflags ^ e.0.rflags) & mask != 0 {
        return Some(format!("flags host={:#x} emu={:#x} (mask {:#x}, diff {:#x})", h.0.rflags & mask, e.0.rflags & mask, mask, (h.0.rflags ^ e.0.rflags) & mask));
    }
    for i in 0..16 {
        if h.0.xmm[i] != e.0.xmm[i] && !lanes_match_modulo_nan(&h.0.xmm[i], &e.0.xmm[i]) {
            return Some(format!(
                "xmm{i} host={:032x} emu={:032x}",
                u128::from_le_bytes(h.0.xmm[i]),
                u128::from_le_bytes(e.0.xmm[i])
            ));
        }
    }
    if asm.starts_with("fxsave") {
        // Compare only the deterministic, architecturally meaningful parts of the 512-byte image:
        // FCW/FSW/FTW (bytes 0..5) and XMM0-15 (160..416). FIP/FDP/MXCSR(sticky bits)/ST0-7 are
        // host-state dependent (the FPU is empty, MXCSR carries exception flags from earlier SSE).
        let o = (SCR + 256 - DATA) as usize;
        let ranges = [0..5, 160..416];
        for r in ranges {
            if h.1[o + r.start..o + r.end] != e.1[o + r.start..o + r.end] {
                return Some(format!("fxsave image bytes {r:?} differ"));
            }
        }
        // Everything outside the fxsave image must be untouched on both sides.
        if h.1[..o] != e.1[..o] || h.1[o + 512..] != e.1[o + 512..] {
            return Some("memory outside the fxsave area differs".into());
        }
        return None;
    }
    if is_x87(asm) {
        if let Some(m) = mem_matches_x87(&h.1, &e.1, x87_tolerant(asm)) {
            return Some(m);
        }
        return None;
    }
    if h.1 != e.1 {
        let k = h.1.iter().zip(&e.1).position(|(a, b)| a != b).unwrap();
        return Some(format!("memory[{k:#x}] host={:#x} emu={:#x}", h.1[k], e.1[k]));
    }
    None
}


fn is_x87(asm: &str) -> bool {
    asm.split(';').any(|s| s.split_whitespace().next().is_some_and(|m| m.starts_with('f') && m != "fxsave" && m != "fxrstor"))
}

/// Transcendental x87 ops: hardware computes in 80-bit extended, this core in f64.
fn x87_tolerant(asm: &str) -> bool {
    ["fsin", "fcos", "fptan", "fpatan", "f2xm1", "fyl2x", "fsincos", "fldl2e", "fldl2t", "fldlg2", "fldln2", "fldpi"]
        .iter()
        .any(|k| asm.split(';').any(|s| s.split_whitespace().next() == Some(k)))
}

fn fill_x87_data(data: &mut [u8], r: &mut Rng) {
    let off = |a: u64| (a - DATA) as usize;
    let val = |r: &mut Rng| -> f64 {
        let v = (r.below(4001) as f64 - 2000.0) / 8.0; // multiples of 1/8: exact in every format
        if r.below(10) == 0 { 0.0 } else if r.below(6) == 0 { -v.abs() } else { v.abs() + 0.125 }
    };
    for slot in [SCR + 16, SCR + 24] {
        data[off(slot)..off(slot) + 8].copy_from_slice(&val(r).to_le_bytes());
    }
    for slot in [SCR + 80, SCR + 84] {
        data[off(slot)..off(slot) + 4].copy_from_slice(&(val(r) as f32).to_le_bytes());
    }
    let i16v = (r.below(2001) as i64 - 1000) as i16;
    data[off(SCR + 96)..off(SCR + 96) + 2].copy_from_slice(&i16v.to_le_bytes());
    let i64v = r.below(1_000_000_000) as i64 - 500_000_000;
    data[off(SCR + 104)..off(SCR + 104) + 8].copy_from_slice(&i64v.to_le_bytes());
    // dword integer operand shares slot +80 with the f32 above: make bytes a valid small int too
    if r.below(2) == 0 {
        let iv = (r.below(100_000) as i32) - 50_000;
        data[off(SCR + 80)..off(SCR + 80) + 4].copy_from_slice(&iv.to_le_bytes());
    }
}

fn mem_matches_x87(h: &[u8], e: &[u8], tolerant: bool) -> Option<String> {
    for k in (0..h.len()).step_by(8) {
        if h[k..k + 8] == e[k..k + 8] {
            continue;
        }
        let (a, b) = (f64::from_le_bytes(h[k..k + 8].try_into().unwrap()), f64::from_le_bytes(e[k..k + 8].try_into().unwrap()));
        if a.is_nan() && b.is_nan() {
            continue;
        }
        if tolerant && (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-300) {
            continue;
        }
        return Some(format!("memory[{k:#x}] host={:02x?} ({a}) emu={:02x?} ({b})", &h[k..k + 8], &e[k..k + 8]));
    }
    None
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

#[test]
fn corpus_matches_host_cpu() {
    setup_host();
    let corpus = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus.txt")).unwrap();
    let filter = std::env::var("DIFF_FILTER").ok();
    let trials: usize = std::env::var("DIFF_TRIALS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let (mut total, mut ran) = (0usize, 0usize);
    for line in corpus.lines() {
        let (hex, asm) = line.split_once('|').unwrap();
        if let Some(f) = &filter {
            if !asm.contains(f.as_str()) {
                continue;
            }
        }
        total += 1;
        if skip(asm).is_some() {
            continue;
        }
        ran += 1;
        let code = unhex(hex);
        let mut rng = Rng(0x1234_5678_9abc_def1 ^ (code.iter().fold(0u64, |a, b| a.wrapping_mul(131).wrapping_add(*b as u64))));
        for t in 0..trials {
            let case = make_case(asm, &mut rng);
            let host = run_host(&code, &case);
            let emu = match run_emu(&code, &case) {
                Ok(v) => v,
                Err(e) => {
                    let ent = failures.entry(asm.to_string()).or_insert((0, format!("trial {t}: emulator error {e}")));
                    ent.0 += 1;
                    continue;
                }
            };
            if let Some(d) = compare(asm, &case, &host, &emu) {
                let ent = failures.entry(asm.to_string()).or_insert((0, format!("trial {t}: {d}")));
                ent.0 += 1;
            }
        }
    }
    eprintln!("differential: {ran} instructions x {trials} trials ({total} in corpus slice), {} failing instruction(s)", failures.len());
    let mut by_mn: BTreeMap<&str, Vec<(&String, &(usize, String))>> = BTreeMap::new();
    for (k, v) in &failures {
        by_mn.entry(mnemonic(k)).or_default().push((k, v));
    }
    let _ = &by_mn;
    for (mn, v) in &by_mn {
        eprintln!("== {mn}: {} failing form(s)", v.len());
        for (k, (n, d)) in v.iter().take(40) {
            eprintln!("   [{n}/{trials}] {k}  -> {d}");
        }
    }
    assert!(failures.is_empty(), "{} instruction forms differ from the host CPU", failures.len());
}

/// Probe (ignored by default): dump host results of 16-bit SHLD/SHRD for counts 17..31 so the
/// hardware's behaviour for the "undefined" range can be fitted. Run with --ignored --nocapture.
#[test]
#[ignore]
fn probe_double_shift16() {
    setup_host();
    let mut rng = Rng(77);
    for (name, code) in [("shld", [0x66u8, 0x0f, 0xa5, 0xd8]), ("shrd", [0x66, 0x0f, 0xad, 0xd8])] {
        // shld/shrd ax, bx, cl
        for _ in 0..40 {
            let mut c = make_case("nop", &mut rng);
            let cnt = 17 + rng.below(15);
            c.st.gpr[RCX] = cnt;
            c.st.gpr[RAX] = rng.next() & 0xffff;
            c.st.gpr[RBX] = rng.next() & 0xffff;
            let (h, _) = run_host(&code, &c);
            println!("{name} dst={:#06x} src={:#06x} cnt={} -> {:#06x} flags={:#x}", c.st.gpr[RAX], c.st.gpr[RBX], cnt, h.gpr[RAX] & 0xffff, h.rflags & ARITH_FLAGS);
        }
    }
}
