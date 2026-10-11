use anywasm_cpu::cpu::{AF, ARITH_FLAGS, CF, DF, OF, PF, RCX, SF, ZF};

pub fn mnemonic(asm: &str) -> &str {
    let mut it = asm.split_whitespace();
    let first = it.next().unwrap();
    if matches!(first, "rep" | "repe" | "repne") {
        it.next().unwrap()
    } else {
        first
    }
}

/// Architecturally defined arithmetic flags to compare for this instruction + operands.
pub fn flag_mask(asm: &str, gpr: &[u64; 16]) -> u64 {
    let m = mnemonic(asm);
    let all = ARITH_FLAGS | DF;
    match m {
        "and" | "or" | "xor" | "test" => all & !AF,
        "shl" | "shr" | "sar" | "shld" | "shrd" => {
            let mut mask = all & !AF;
            let count = shift_count(asm, gpr);
            if count == 0 {
                mask = all; // flags untouched: must match exactly
            } else if count != 1 {
                mask &= !OF;
            }
            mask
        }
        "rol" | "ror" | "rcl" | "rcr" => {
            let count = shift_count(asm, gpr);
            if count == 1 || count == 0 {
                all
            } else {
                all & !OF
            }
        }
        "mul" | "imul" => all & !(SF | ZF | AF | PF),
        "bzhi" => all & !(AF | PF), // AF and PF are undefined for BZHI.
        "div" | "idiv" => DF,
        "bsf" | "bsr" => ZF | DF,
        "tzcnt" | "lzcnt" => (CF | ZF) | DF,
        "bt" | "bts" | "btr" | "btc" => (CF | ZF) | DF,
        _ => all,
    }
}

fn shift_count(asm: &str, gpr: &[u64; 16]) -> u64 {
    let last = asm.rsplit(',').next().unwrap().trim();
    let bits: u64 = if asm.contains("qword")
        || asm.contains(" r")
            && !asm.contains("byte")
            && !asm.contains("dword")
            && !asm.contains("word")
            && uses_r64(asm)
    {
        63
    } else {
        31
    };
    let raw = if last == "cl" {
        gpr[RCX] & 0xff
    } else {
        last.parse::<u64>().unwrap_or(1)
    };
    raw & bits
}

fn uses_r64(asm: &str) -> bool {
    let ops = asm.splitn(2, ' ').nth(1).unwrap_or("");
    let first = ops.split(',').next().unwrap().trim();
    first.starts_with('r')
        && !first.ends_with('d')
        && !first.ends_with('w')
        && !first.ends_with('b')
}

/// Results that legitimately differ from the host CPU or are nondeterministic.
pub fn skip(asm: &str) -> Option<&'static str> {
    asm.split(';')
        .map(str::trim)
        .filter(|instruction| !instruction.is_empty())
        .find_map(|instruction| match mnemonic(instruction) {
            "rdtsc" | "rdrand" | "cpuid" => Some("nondeterministic"),
            "rcpps" | "rsqrtps" | "rcpss" | "rsqrtss" => Some("hardware approximation"),
            _ => None,
        })
}
