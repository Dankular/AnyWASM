//! Focused D3DX9.31 helpers used by Sims 3.
//!
//! Shader-size queries are decoded from the guest token stream. Volume-texture filling remains
//! an explicit coverage gap until the D3D9 model has volume textures, LockBox, and guest callback
//! invocation support.

use super::*;
use crate::process::{Process, Stop};

const D3DERR_INVALIDCALL: u64 = 0x8876_086c;
const MAX_SHADER_DWORDS: usize = 65_536;

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "D3DXGetShaderSize" => get_shader_size,
        "D3DXFillVolumeTexture" => fill_volume_texture,
        _ => return None,
    })
}

/// x86 WINAPI stack cleanup for the two imported D3DX exports.
pub fn stdcall_stack_bytes(name: &str) -> Option<u64> {
    match name {
        "D3DXGetShaderSize" => Some(4),
        "D3DXFillVolumeTexture" => Some(12),
        _ => None,
    }
}

fn get_shader_size(p: &mut Process) -> R {
    shader_size(p, p.arg(0)).map(|size| size as u64)
}

fn shader_size(p: &Process, shader: u64) -> Result<u32, Stop> {
    if shader == 0 {
        return Err(Stop::Crash(
            "D3DXGetShaderSize dereferenced a null shader bytecode pointer".into(),
        ));
    }

    let version = p.rd32(shader)?;
    let shader_type = version >> 16;
    if shader_type != 0xfffe && shader_type != 0xffff {
        return Err(unimplemented(format!(
            "invalid shader version token {version:#010x}"
        )));
    }
    let major = ((version >> 8) & 0xff) as u8;
    let minor = (version & 0xff) as u8;
    if !(1..=3).contains(&major) {
        return Err(unimplemented(format!(
            "unsupported shader model {major}.{minor}"
        )));
    }

    let pixel_shader = shader_type == 0xffff;
    let mut cursor = shader
        .checked_add(4)
        .ok_or_else(|| unimplemented("shader pointer overflow"))?;
    let mut size = 4usize; // version token

    for _ in 1..MAX_SHADER_DWORDS {
        let instruction = p.rd32(cursor)?;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| unimplemented("shader pointer overflow"))?;
        size += 4;

        let opcode = instruction & 0xffff;
        if opcode == 0xffff {
            return u32::try_from(size).map_err(|_| unimplemented("shader byte count overflow"));
        }

        let operand_dwords = match opcode {
            0xfffe => ((instruction >> 16) & 0x7fff) as usize, // D3DSIO_COMMENT
            0xfffd => 0,                                       // D3DSIO_PHASE (ps_1_4)
            _ if major >= 2 => ((instruction >> 24) & 0x0f) as usize,
            _ => legacy_operand_dwords(opcode, pixel_shader, minor).ok_or_else(|| {
                unimplemented(format!(
                    "shader model {major}.{minor} opcode {opcode:#x} has no verified token layout"
                ))
            })?,
        };
        let operand_bytes = operand_dwords
            .checked_mul(4)
            .ok_or_else(|| unimplemented("shader byte count overflow"))?;
        cursor = cursor
            .checked_add(operand_bytes as u64)
            .ok_or_else(|| unimplemented("shader pointer overflow"))?;
        size = size
            .checked_add(operand_bytes)
            .ok_or_else(|| unimplemented("shader byte count overflow"))?;
    }

    Err(unimplemented(format!(
        "shader stream has no END token within {MAX_SHADER_DWORDS} DWORDs"
    )))
}

/// Number of parameter DWORDs for the legacy shader model 1 instruction encoding.
///
/// Shader models 2 and 3 carry the parameter count in each instruction token and do not use this
/// table. Unknown model 1 opcodes are reported as coverage gaps instead of guessed sizes.
fn legacy_operand_dwords(opcode: u32, pixel: bool, minor: u8) -> Option<usize> {
    let operands = if pixel {
        match opcode {
            0 | 28 | 29 | 42 | 43 | 44 => 0, // nop, ret/endloop/else/endif/break
            1 | 6 | 7 | 14 | 15 | 16 | 19 | 35 | 36 | 77 | 78 => 2,
            2 | 3 | 5 | 8 | 9 | 10 | 11 | 12 | 13 | 17 | 84 => 3,
            4 | 18 | 79 | 87 => 4,
            64..=76 | 81..=86 | 88 => 1,
            80 => 5, // DEF: destination register and four immediate constants
            _ => return None,
        }
    } else {
        match opcode {
            0 | 28 | 29 => 0, // nop, ret, endloop
            1 | 6 | 7 | 14 | 15 | 16 | 19 | 35 | 36 => 2,
            2 | 3 | 5 | 8..=13 | 17 | 20..=24 | 32 | 33 => 3,
            4 | 18 | 34 => 4,
            25 | 30 => 1,      // call, label
            26 | 27 | 31 => 2, // callnz, loop, dcl
            _ => return None,
        }
    };

    // In the ps_1_4 profile, TEX is the explicit two-operand TEXLD instruction. Earlier ps_1.x
    // profiles encode TEX with its destination texture register only.
    Some(if pixel && minor >= 4 && opcode == 66 {
        2
    } else {
        operands
    })
}

fn fill_volume_texture(p: &mut Process) -> R {
    let (texture, callback) = (p.arg(0), p.arg(1));
    if texture == 0 || callback == 0 {
        return Ok(D3DERR_INVALIDCALL);
    }
    Err(unimplemented(
        "D3DXFillVolumeTexture (IDirect3DVolumeTexture9, LockBox, and guest callback execution are not modeled)",
    ))
}

fn unimplemented(name: impl Into<String>) -> Stop {
    Stop::Unimplemented {
        dll: "d3dx9_31".into(),
        name: name.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::{
        cpu::RSP,
        mem::{PAGE_SIZE, PROT_RW},
        CpuMode,
    };

    const CODE: u64 = 0x10000;
    const STACK: u64 = 0x20000;

    fn x86_process() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        for base in [CODE, STACK] {
            assert!(process.mem.map(base, PAGE_SIZE, PROT_RW));
        }
        process.cpu.gpr[RSP] = STACK;
        process.wr32(STACK + 4, CODE as u32).unwrap();
        process
    }

    fn run_shader(words: &[u32]) -> Result<u64, Stop> {
        let mut process = x86_process();
        for (index, word) in words.iter().enumerate() {
            process.wr32(CODE + index as u64 * 4, *word).unwrap();
        }
        lookup("D3DXGetShaderSize").unwrap()(&mut process)
    }

    #[test]
    fn shader_size_counts_model_1_vertex_operands_and_end_token() {
        // vs_1_1; mov r0, v0; end
        assert_eq!(
            run_shader(&[
                0xfffe_0101,
                0x0000_0001,
                0x8000_0000,
                0x900f_0000,
                0x0000_ffff
            ])
            .unwrap(),
            20
        );
    }

    #[test]
    fn shader_size_counts_model_2_lengths_and_comment_payload() {
        // vs_2_0; mov with a two-DWORD operand length; two-DWORD comment; end.
        assert_eq!(
            run_shader(&[
                0xfffe_0200,
                0x0200_0001,
                0x8000_0000,
                0x900f_0000,
                0x0002_fffe,
                0x1122_3344,
                0x5566_7788,
                0x0000_ffff,
            ])
            .unwrap(),
            32
        );
    }

    #[test]
    fn shader_size_uses_two_operands_for_ps_1_4_texld() {
        assert_eq!(
            run_shader(&[
                0xffff_0104,
                0x0000_0042,
                0x8000_0000,
                0xb00f_0000,
                0x0000_ffff
            ])
            .unwrap(),
            20
        );
    }

    #[test]
    fn unknown_model_1_instruction_reports_an_explicit_coverage_gap() {
        let result = run_shader(&[0xfffe_0101, 0x0000_002f]);
        assert!(matches!(
            result,
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "d3dx9_31" && name.contains("opcode 0x2f")
        ));
    }

    #[test]
    fn invalid_shader_tokens_do_not_report_a_successful_zero_size() {
        assert!(matches!(
            run_shader(&[0]),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "d3dx9_31" && name.contains("invalid shader version token")
        ));
    }

    #[test]
    fn volume_fill_reports_missing_volume_texture_backing() {
        let mut process = x86_process();
        process.wr32(STACK + 4, 0x30000).unwrap();
        process.wr32(STACK + 8, 0x31000).unwrap();
        process.wr32(STACK + 12, 0).unwrap();
        assert_eq!(stdcall_stack_bytes("D3DXFillVolumeTexture"), Some(12));
        assert!(matches!(
            lookup("D3DXFillVolumeTexture").unwrap()(&mut process),
            Err(Stop::Unimplemented { ref dll, ref name })
                if dll == "d3dx9_31" && name.contains("VolumeTexture")
        ));
    }

    #[test]
    fn volume_fill_rejects_null_texture_or_callback() {
        let mut process = x86_process();
        process.wr32(STACK + 4, 0).unwrap();
        process.wr32(STACK + 8, 0x31000).unwrap();
        process.wr32(STACK + 12, 0).unwrap();
        assert_eq!(
            lookup("D3DXFillVolumeTexture").unwrap()(&mut process).unwrap(),
            D3DERR_INVALIDCALL
        );
        process.wr32(STACK + 4, 0x30000).unwrap();
        process.wr32(STACK + 8, 0).unwrap();
        assert_eq!(
            lookup("D3DXFillVolumeTexture").unwrap()(&mut process).unwrap(),
            D3DERR_INVALIDCALL
        );
    }
}
