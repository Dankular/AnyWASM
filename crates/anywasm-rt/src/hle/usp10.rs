//! A basic-Latin subset of the Uniscribe text-analysis entry points imported by Sims 3.
//!
//! The runtime does not ship a complete Unicode bidi or shaping engine. These implementations
//! handle neutral Basic Latin text with the generic `SCRIPT_UNDEFINED` analysis; scripts and
//! shaping controls outside that subset stop explicitly rather than returning fabricated runs.

use crate::process::{HleFn, Process, Stop};

const E_FAIL: u64 = 0x8000_4005;
const E_INVALIDARG: u64 = 0x8007_0057;
const MAX_TEXT_UNITS: usize = 1 << 20;

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "ScriptItemize" => Some(script_itemize),
        "ScriptBreak" => Some(script_break),
        _ => None,
    }
}

/// x86 WINAPI stack cleanup for the imported Uniscribe entry points.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    match name {
        "ScriptItemize" => Some(28),
        "ScriptBreak" => Some(16),
        _ => None,
    }
}

fn unsupported(name: impl Into<String>) -> Stop {
    Stop::Unimplemented {
        dll: "usp10".into(),
        name: name.into(),
    }
}

fn read_text(p: &Process, address: u64, count: usize) -> Result<Vec<u16>, Stop> {
    if count > MAX_TEXT_UNITS {
        return Err(unsupported(format!(
            "text analysis length {count} exceeds the supported limit"
        )));
    }
    if count != 0 && address == 0 {
        return Err(unsupported("null text buffer with nonzero character count"));
    }
    (0..count)
        .map(|index| {
            let offset = u64::try_from(index)
                .ok()
                .and_then(|n| n.checked_mul(2))
                .and_then(|n| address.checked_add(n))
                .ok_or_else(|| Stop::Crash("Uniscribe text pointer overflow".into()))?;
            p.rd16(offset)
        })
        .collect()
}

fn ensure_basic_latin(text: &[u16], api: &str) -> Result<(), Stop> {
    if let Some((index, character)) = text
        .iter()
        .copied()
        .enumerate()
        .find(|(_, character)| *character > 0x007f)
    {
        return Err(unsupported(format!(
            "{api} does not yet implement non-Basic-Latin character U+{character:04X} at UTF-16 index {index}"
        )));
    }
    Ok(())
}

fn script_itemize(p: &mut Process) -> Result<u64, Stop> {
    let input = p.arg(0);
    let input_count = p.arg(1) as u32 as i32;
    let max_items = p.arg(2) as u32 as i32;
    let control = p.arg(3);
    let initial_state = p.arg(4);
    let items = p.arg(5);
    let item_count = p.arg(6);

    if input_count <= 0 || max_items < 1 || items == 0 || item_count == 0 || input == 0 {
        return Ok(E_INVALIDARG);
    }

    // This subset emits one neutral generic-shaping item and does not interpret bidi controls.
    // Accept only the all-zero controls/state represented by this handler's item analysis.
    if control != 0 && p.rd32(control)? != 0 {
        return Err(unsupported(
            "ScriptItemize non-default SCRIPT_CONTROL options",
        ));
    }
    if initial_state != 0 && p.rd16(initial_state)? != 0 {
        return Err(unsupported(
            "ScriptItemize non-default initial SCRIPT_STATE",
        ));
    }

    let text = read_text(p, input, input_count as usize)?;
    ensure_basic_latin(&text, "ScriptItemize")?;

    // SCRIPT_ITEM is { LONG iCharPos; SCRIPT_ANALYSIS a; }, with each analysis occupying
    // two WORDs. SCRIPT_UNDEFINED and the zeroed state disable script-specific shaping.
    let item_stride = 8u64;
    let terminal_index = if text.is_empty() { 0 } else { 1 };
    for index in 0..=terminal_index {
        let position = if index == 0 { 0 } else { text.len() };
        let address = items
            .checked_add(index as u64 * item_stride)
            .ok_or_else(|| Stop::Crash("SCRIPT_ITEM output pointer overflow".into()))?;
        p.wr32(address, position as u32)?;
        p.wr32(address + 4, 0)?;
        p.wr16(address + 6, 0)?;
    }
    p.wr32(item_count, if text.is_empty() { 0 } else { 1 })?;
    Ok(0)
}

fn script_break(p: &mut Process) -> Result<u64, Stop> {
    let text_address = p.arg(0);
    let text_count = p.arg(1) as u32 as i32;
    let analysis = p.arg(2);
    let attributes = p.arg(3);

    if text_count < 0 || analysis == 0 || (text_count > 0 && (text_address == 0 || attributes == 0))
    {
        return Ok(E_INVALIDARG);
    }
    if text_count == 0 {
        return Ok(E_FAIL);
    }

    // ScriptBreak only accepts the SCRIPT_UNDEFINED / base-LTR analysis produced above.
    // Reject every script, direction, link, or Bidi-state flag this subset cannot honor.
    let analysis_bits = p.rd16(analysis)?;
    let state_bits = p.rd16(analysis + 2)?;
    if analysis_bits != 0 || state_bits != 0 {
        return Err(unsupported(format!(
            "ScriptBreak SCRIPT_ANALYSIS {analysis_bits:#06x}:{state_bits:#06x}"
        )));
    }

    let text = read_text(p, text_address, text_count as usize)?;
    ensure_basic_latin(&text, "ScriptBreak")?;

    // For this neutral Basic-Latin run every code unit is a legal caret stop. The generic
    // engine has no script-specific word or soft-break flags; breakable ASCII whitespace is
    // reported in fWhiteSpace (bit 1), as SCRIPT_LOGATTR defines.
    for (index, unit) in text.iter().copied().enumerate() {
        let whitespace = matches!(unit, 0x0009..=0x000d | 0x0020);
        let attr = 0x04 | if whitespace { 0x02 } else { 0 };
        let address = attributes
            .checked_add(index as u64)
            .ok_or_else(|| Stop::Crash("SCRIPT_LOGATTR output pointer overflow".into()))?;
        p.wr8(address, attr)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::{
        cpu::{CpuMode, RSP},
        mem::{PAGE_SIZE, PROT_RW},
    };

    const STACK: u64 = 0x10000;
    const INPUT: u64 = 0x20000;
    const ITEMS: u64 = 0x21000;
    const COUNT: u64 = 0x22000;
    const ANALYSIS: u64 = 0x23000;
    const ATTRIBUTES: u64 = 0x24000;

    fn x86_process() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        for base in [STACK, INPUT, ITEMS, COUNT, ANALYSIS, ATTRIBUTES] {
            assert!(process.mem.map(base, PAGE_SIZE, PROT_RW));
        }
        process.cpu.gpr[RSP] = STACK;
        process
    }

    fn set_args(process: &mut Process, args: &[u32]) {
        for (index, value) in args.iter().enumerate() {
            process.wr32(STACK + 4 + 4 * index as u64, *value).unwrap();
        }
    }

    fn write_text(process: &mut Process, text: &[u16]) {
        for (index, unit) in text.iter().copied().enumerate() {
            process.wr16(INPUT + 2 * index as u64, unit).unwrap();
        }
    }

    #[test]
    fn x86_stdcall_sizes_match_import_signatures() {
        assert_eq!(stdcall_stack_bytes("ScriptItemize"), Some(28));
        assert_eq!(stdcall_stack_bytes("ScriptBreak"), Some(16));
        assert_eq!(stdcall_stack_bytes("other"), None);
    }

    #[test]
    fn itemize_returns_one_generic_ltr_item_and_terminal_position() {
        let mut process = x86_process();
        write_text(&mut process, &[b'A' as u16, b' ' as u16, b'B' as u16]);
        set_args(
            &mut process,
            &[INPUT as u32, 3, 8, 0, 0, ITEMS as u32, COUNT as u32],
        );

        assert_eq!(script_itemize(&mut process).unwrap(), 0);
        assert_eq!(process.rd32(COUNT).unwrap(), 1);
        assert_eq!(process.rd32(ITEMS).unwrap(), 0);
        assert_eq!(process.rd32(ITEMS + 4).unwrap(), 0); // SCRIPT_UNDEFINED, LTR
        assert_eq!(process.rd16(ITEMS + 6).unwrap(), 0); // initial SCRIPT_STATE
        assert_eq!(process.rd32(ITEMS + 8).unwrap(), 3); // terminal item
        assert_eq!(process.rd32(ITEMS + 12).unwrap(), 0);
    }

    #[test]
    fn itemize_empty_input_is_invalid_and_does_not_claim_success() {
        let mut process = x86_process();
        set_args(&mut process, &[0, 0, 1, 0, 0, ITEMS as u32, COUNT as u32]);
        assert_eq!(script_itemize(&mut process).unwrap(), E_INVALIDARG);
    }

    #[test]
    fn itemize_rejects_complex_unicode_with_an_explicit_coverage_gap() {
        let mut process = x86_process();
        write_text(&mut process, &[0x05d0]); // Hebrew alef requires bidi/script analysis.
        set_args(
            &mut process,
            &[INPUT as u32, 1, 8, 0, 0, ITEMS as u32, COUNT as u32],
        );
        assert!(matches!(
            script_itemize(&mut process),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "usp10" && name.contains("U+05D0")
        ));
    }

    #[test]
    fn break_sets_char_stops_and_breakable_ascii_whitespace() {
        let mut process = x86_process();
        write_text(&mut process, &[b' ' as u16, b'A' as u16, b'\n' as u16]);
        set_args(
            &mut process,
            &[INPUT as u32, 3, ANALYSIS as u32, ATTRIBUTES as u32],
        );

        assert_eq!(script_break(&mut process).unwrap(), 0);
        assert_eq!(process.rd8(ATTRIBUTES).unwrap(), 0x06);
        assert_eq!(process.rd8(ATTRIBUTES + 1).unwrap(), 0x04);
        assert_eq!(process.rd8(ATTRIBUTES + 2).unwrap(), 0x06);
    }

    #[test]
    fn break_rejects_unicode_and_nondefault_analysis() {
        let mut process = x86_process();
        write_text(&mut process, &[0x0301]);
        set_args(
            &mut process,
            &[INPUT as u32, 1, ANALYSIS as u32, ATTRIBUTES as u32],
        );
        assert!(matches!(
            script_break(&mut process),
            Err(Stop::Unimplemented { ref dll, .. }) if dll == "usp10"
        ));

        process.wr16(ANALYSIS, 1).unwrap();
        write_text(&mut process, &[b'A' as u16]);
        assert!(matches!(
            script_break(&mut process),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "usp10" && name.contains("SCRIPT_ANALYSIS")
        ));
    }

    #[test]
    fn itemize_invalid_capacity_returns_hresult_instead_of_success() {
        let mut process = x86_process();
        write_text(&mut process, &[b'A' as u16]);
        set_args(
            &mut process,
            &[INPUT as u32, 1, 0, 0, 0, ITEMS as u32, COUNT as u32],
        );
        assert_eq!(script_itemize(&mut process).unwrap(), E_INVALIDARG);
    }
}
