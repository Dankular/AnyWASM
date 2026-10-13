mod combined_samplers;
mod draw_parameters;

use wasm_bindgen::prelude::*;

const MAX_SPIRV_BYTES: usize = 4 * 1024 * 1024;
const SPIRV_MAGIC_LE: [u8; 4] = [0x03, 0x02, 0x23, 0x07];

/// Converts a complete little-endian SPIR-V module to WGSL for WebGPU.
/// The D3D-to-SPIR-V stage assigns bindings; this library never guesses them.
#[wasm_bindgen]
pub fn spirv_to_wgsl(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() < 20 || bytes.len() > MAX_SPIRV_BYTES || bytes.len() % 4 != 0 {
        return Err("SPIR-V input must be 20 bytes to 4 MiB and DWORD-aligned".into());
    }
    if !bytes.starts_with(&SPIRV_MAGIC_LE) {
        return Err("SPIR-V input has no little-endian magic word".into());
    }

    let normalized = draw_parameters::lower(bytes)?;
    emit_wgsl(&normalized, false)
}

/// Vulkan resources keep descriptor set numbers; binding N maps to 2*N and
/// a combined image sampler maps to image 2*N plus sampler 2*N+1.
#[wasm_bindgen]
pub fn vulkan_spirv_to_wgsl(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() < 20
        || bytes.len() > MAX_SPIRV_BYTES
        || bytes.len() % 4 != 0
        || !bytes.starts_with(&SPIRV_MAGIC_LE)
    {
        return Err("Invalid bounded Vulkan SPIR-V module".into());
    }
    let mut words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let mut at = 5;
    while at < words.len() {
        let count = (words[at] >> 16) as usize;
        if count == 0 || at + count > words.len() {
            return Err("Invalid SPIR-V instruction length".into());
        }
        if words[at] & 65535 == 71 && count == 4 && words[at + 2] == 33 {
            if words[at + 3] >= 32 {
                return Err("Vulkan descriptor binding exceeds 31".into());
            }
            words[at + 3] *= 2;
        }
        at += count;
    }
    let doubled: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let (separate, _) = combined_samplers::split_with_bindings(&doubled)?;
    emit_wgsl(&separate, true)
}

fn emit_wgsl(bytes: &[u8], adjust_coordinate_space: bool) -> Result<String, String> {
    // D3D and WebGPU both use depth 0..1. The renderer handles viewport
    // orientation and winding, so Naga must not adjust the clip coordinates.
    let options = naga::front::spv::Options {
        adjust_coordinate_space,
        ..Default::default()
    };
    let mut module = naga::front::spv::parse_u8_slice(bytes, &options)
        .map_err(|error| format!("SPIR-V parse failed: {error}"))?;
    if adjust_coordinate_space {
        // WebGPU has no push constants. Reserve group 3 for a submission-time
        // immutable uniform snapshot of the Vulkan command buffer's bytes.
        for (_, variable) in module.global_variables.iter_mut() {
            if variable.space == naga::AddressSpace::Immediate {
                variable.space = naga::AddressSpace::Uniform;
                variable.binding = Some(naga::ResourceBinding { group: 3, binding: 0 });
            }
        }
    }
    if module.entry_points.is_empty() {
        return Err("SPIR-V module has no shader entry point".into());
    }
    let validator = || {
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
    };
    let information = validator()
        .validate(&module)
        .map_err(|error| format!("SPIR-V validation failed: {error}"))?;
    let wgsl = naga::back::wgsl::write_string(
        &module,
        &information,
        naga::back::wgsl::WriterFlags::empty(),
    )
    .map_err(|error| format!("WGSL emission failed: {error}"))?;

    let reparsed = naga::front::wgsl::parse_str(&wgsl).map_err(|error| {
        format!(
            "Generated WGSL parse failed: {}",
            error.emit_to_string(&wgsl)
        )
    })?;
    validator()
        .validate(&reparsed)
        .map_err(|error| format!("Generated WGSL validation failed: {error}"))?;
    Ok(wgsl)
}
