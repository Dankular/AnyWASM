//! C-ABI bridge for quiescent guest-memory snapshots.

use anywasm_cpu_state::shared::MappingEntry;

const MAPPING_ENTRY_BYTES: usize = 16;
const MAPPING_ENTRY_ALIGN: usize = 8;
const GUEST_PAGE_BYTES: usize = 4096;
const _: [(); MAPPING_ENTRY_BYTES] = [(); std::mem::size_of::<MappingEntry>()];

fn ranges_overlap(left: *const u8, left_len: usize, right: *const u8, right_len: usize) -> bool {
    if left_len == 0 || right_len == 0 {
        return false;
    }
    let left_start = left as usize;
    let right_start = right as usize;
    let Some(left_end) = left_start.checked_add(left_len) else {
        return true;
    };
    let Some(right_end) = right_start.checked_add(right_len) else {
        return true;
    };
    left_start < right_end && right_start < left_end
}

/// Number of mapped guest pages in the current process, or `u64::MAX` when no state is loaded.
#[no_mangle]
pub extern "C" fn aw_shared_snapshot_page_count() -> u64 {
    super::state()
        .map(|s| s.p.mem.pages_mapped())
        .unwrap_or(u64::MAX)
}

/// Return the mapping/protection generation, or `u32::MAX` when no state is loaded.
#[no_mangle]
pub extern "C" fn aw_shared_snapshot_mapping_generation() -> u32 {
    super::state()
        .map(|s| s.p.mem.shared_mapping_generation())
        .unwrap_or(u32::MAX)
}

/// Export a sorted mapping table and packed guest frames for publication to a shared-memory worker.
///
/// `mapping_capacity_bytes` and `frame_capacity_bytes` are byte capacities. The mapping buffer
/// contains `page_count` 16-byte `MappingEntry` records and the frame buffer contains one 4096-byte
/// record per mapped guest page. `frame_base` is the shared-memory byte offset where the caller will
/// place the packed frame buffer. Call only while guest execution is quiescent.
/// Returns 0 on success, 1 without a loaded state, 2 for insufficient capacity, 3 for invalid
/// pointers or size overflow, and 4 for a snapshot validation error.
///
/// # Safety
/// Non-null output pointers must point to writable buffers of the supplied capacities. The mapping
/// pointer must be aligned to 8 bytes.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_snapshot_export(
    frame_base: u32,
    mappings_ptr: *mut u8,
    mapping_capacity_bytes: usize,
    frames_ptr: *mut u8,
    frame_capacity_bytes: usize,
) -> i32 {
    let Some(s) = super::state() else { return 1 };
    if frame_base % GUEST_PAGE_BYTES as u32 != 0 {
        s.message = b"shared snapshot frame base is not page-aligned".to_vec();
        return 3;
    }
    let Ok(page_count) = usize::try_from(s.p.mem.pages_mapped()) else {
        s.message = b"shared snapshot page count overflow".to_vec();
        return 3;
    };
    let Some(mapping_bytes) = page_count.checked_mul(MAPPING_ENTRY_BYTES) else {
        s.message = b"shared snapshot mapping size overflow".to_vec();
        return 3;
    };
    let Some(frame_bytes) = page_count.checked_mul(GUEST_PAGE_BYTES) else {
        s.message = b"shared snapshot frame size overflow".to_vec();
        return 3;
    };
    if mapping_capacity_bytes < mapping_bytes || frame_capacity_bytes < frame_bytes {
        s.message = b"shared snapshot output buffer is too small".to_vec();
        return 2;
    }
    if mappings_ptr.is_null()
        || frames_ptr.is_null()
        || mappings_ptr as usize % MAPPING_ENTRY_ALIGN != 0
        || ranges_overlap(
            mappings_ptr.cast_const(),
            mapping_bytes,
            frames_ptr,
            frame_bytes,
        )
    {
        s.message = b"invalid shared snapshot output buffer".to_vec();
        return 3;
    }

    let mappings = std::slice::from_raw_parts_mut(mappings_ptr.cast::<MappingEntry>(), page_count);
    let frames = std::slice::from_raw_parts_mut(frames_ptr, frame_bytes);
    match s.p.mem.export_shared_snapshot(frame_base, mappings, frames) {
        Ok(()) => {
            s.message.clear();
            0
        }
        Err(error) => {
            s.message = format!("shared snapshot export failed: {error:?}").into_bytes();
            4
        }
    }
}

/// Import worker-written frames after all workers have reached a safe point.
///
/// The mapping buffer must be unchanged from the export for this mapping generation. Mapping bytes
/// must equal `page_count * 16`; the frame buffer must contain at least one page per record. Returns
/// 0 on success, 1 without a loaded state, 2 for incorrect capacity, 3 for invalid pointers,
/// alignment, or size overflow, and 4 if the mappings do not match.
///
/// # Safety
/// Non-null input pointers must point to readable buffers of the supplied capacities. The mapping
/// pointer must be aligned to 8 bytes.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_snapshot_import(
    frame_base: u32,
    mappings_ptr: *const u8,
    mapping_bytes: usize,
    frames_ptr: *const u8,
    frame_capacity_bytes: usize,
) -> i32 {
    let Some(s) = super::state() else { return 1 };
    let Ok(page_count) = usize::try_from(s.p.mem.pages_mapped()) else {
        s.message = b"shared snapshot page count overflow".to_vec();
        return 3;
    };
    let Some(expected_mapping_bytes) = page_count.checked_mul(MAPPING_ENTRY_BYTES) else {
        s.message = b"shared snapshot mapping size overflow".to_vec();
        return 3;
    };
    let Some(expected_frame_bytes) = page_count.checked_mul(GUEST_PAGE_BYTES) else {
        s.message = b"shared snapshot frame size overflow".to_vec();
        return 3;
    };
    if mapping_bytes != expected_mapping_bytes || frame_capacity_bytes < expected_frame_bytes {
        s.message = b"shared snapshot input buffer has the wrong size".to_vec();
        return 2;
    }
    if mappings_ptr.is_null()
        || frames_ptr.is_null()
        || mappings_ptr as usize % MAPPING_ENTRY_ALIGN != 0
        || ranges_overlap(
            mappings_ptr,
            mapping_bytes,
            frames_ptr,
            expected_frame_bytes,
        )
    {
        s.message = b"invalid shared snapshot input buffer".to_vec();
        return 3;
    }

    let mappings = std::slice::from_raw_parts(mappings_ptr.cast::<MappingEntry>(), page_count);
    let frames = std::slice::from_raw_parts(frames_ptr, expected_frame_bytes);
    match s.p.mem.import_shared_snapshot(frame_base, mappings, frames) {
        Ok(()) => {
            s.message.clear();
            0
        }
        Err(error) => {
            s.message = format!("shared snapshot import failed: {error:?}").into_bytes();
            4
        }
    }
}

/// Export only host-dirty frames. `dirty_pages` contains sorted indices into the last full mapping
/// snapshot; `dirty_frames` contains the corresponding dense page bytes.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_snapshot_export_delta(
    frame_base: u32,
    dirty_pages_ptr: *mut u8,
    dirty_pages_capacity_bytes: usize,
    dirty_frames_ptr: *mut u8,
    dirty_frames_capacity_bytes: usize,
    dirty_count_ptr: *mut u32,
) -> i32 {
    let Some(s) = super::state() else { return 1 };
    if frame_base % GUEST_PAGE_BYTES as u32 != 0 {
        s.message = b"shared snapshot frame base is not page-aligned".to_vec();
        return 3;
    }
    let Ok(page_count) = usize::try_from(s.p.mem.pages_mapped()) else {
        s.message = b"shared snapshot page count overflow".to_vec();
        return 3;
    };
    let Some(dirty_pages_bytes) = page_count.checked_mul(core::mem::size_of::<u32>()) else {
        s.message = b"shared snapshot dirty-page list size overflow".to_vec();
        return 3;
    };
    let Some(frame_bytes) = page_count.checked_mul(GUEST_PAGE_BYTES) else {
        s.message = b"shared snapshot frame size overflow".to_vec();
        return 3;
    };
    if dirty_pages_capacity_bytes < dirty_pages_bytes || dirty_frames_capacity_bytes < frame_bytes {
        s.message = b"shared snapshot delta output buffer is too small".to_vec();
        return 2;
    }
    if dirty_pages_ptr.is_null()
        || dirty_frames_ptr.is_null()
        || dirty_count_ptr.is_null()
        || dirty_pages_ptr as usize % core::mem::align_of::<u32>() != 0
        || dirty_count_ptr as usize % core::mem::align_of::<u32>() != 0
        || ranges_overlap(
            dirty_pages_ptr,
            dirty_pages_bytes,
            dirty_frames_ptr,
            frame_bytes,
        )
        || ranges_overlap(
            dirty_count_ptr.cast(),
            4,
            dirty_pages_ptr,
            dirty_pages_bytes,
        )
        || ranges_overlap(dirty_count_ptr.cast(), 4, dirty_frames_ptr, frame_bytes)
    {
        s.message = b"invalid shared snapshot delta output buffers".to_vec();
        return 3;
    }

    let dirty_pages = std::slice::from_raw_parts_mut(dirty_pages_ptr.cast::<u32>(), page_count);
    let dirty_frames = std::slice::from_raw_parts_mut(dirty_frames_ptr, frame_bytes);
    match s.p.mem.export_shared_delta(dirty_pages, dirty_frames) {
        Ok(dirty_count) => {
            *dirty_count_ptr = dirty_count as u32;
            s.message.clear();
            0
        }
        Err(error) => {
            s.message = format!("shared snapshot delta export failed: {error:?}").into_bytes();
            4
        }
    }
}

/// Acknowledge host-dirty mapping rows after their bytes have been published to workers.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_snapshot_clear_dirty_pages(
    dirty_pages_ptr: *const u32,
    dirty_page_count: usize,
) -> i32 {
    let Some(s) = super::state() else { return 1 };
    if dirty_page_count != 0
        && (dirty_pages_ptr.is_null()
            || dirty_pages_ptr as usize % core::mem::align_of::<u32>() != 0)
    {
        return 3;
    }
    let dirty_pages = if dirty_page_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(dirty_pages_ptr, dirty_page_count)
    };
    match s.p.mem.clear_shared_dirty_pages(dirty_pages) {
        Ok(()) => 0,
        Err(error) => {
            s.message = format!("shared snapshot dirty-page acknowledgement failed: {error:?}")
                .into_bytes();
            4
        }
    }
}

/// Acknowledge all host-dirty pages after a complete snapshot has been published.
#[no_mangle]
pub extern "C" fn aw_shared_snapshot_clear_all_dirty() -> i32 {
    let Some(s) = super::state() else { return 1 };
    s.p.mem.clear_all_shared_dirty();
    0
}

/// Import only worker-dirty pages after every shared worker has reached a safe point.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_snapshot_import_dirty(
    mapping_generation: u32,
    frame_base: u32,
    dirty_pages_ptr: *const u32,
    dirty_page_count: usize,
    dirty_frames_ptr: *const u8,
    dirty_frames_capacity_bytes: usize,
) -> i32 {
    let Some(s) = super::state() else { return 1 };
    let Ok(page_count) = usize::try_from(s.p.mem.pages_mapped()) else {
        return 3;
    };
    let Some(dirty_pages_bytes) = dirty_page_count.checked_mul(core::mem::size_of::<u32>()) else {
        return 3;
    };
    let Some(expected_dirty_frame_bytes) = dirty_page_count.checked_mul(GUEST_PAGE_BYTES) else {
        return 3;
    };
    if dirty_page_count > page_count || dirty_frames_capacity_bytes < expected_dirty_frame_bytes {
        return 2;
    }
    if dirty_frames_ptr.is_null()
        || (dirty_page_count != 0
            && (dirty_pages_ptr.is_null()
                || dirty_pages_ptr as usize % core::mem::align_of::<u32>() != 0))
        || ranges_overlap(
            dirty_pages_ptr.cast(),
            dirty_pages_bytes,
            dirty_frames_ptr,
            expected_dirty_frame_bytes,
        )
    {
        return 3;
    }
    let dirty_pages = if dirty_page_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(dirty_pages_ptr, dirty_page_count)
    };
    let dirty_frames = std::slice::from_raw_parts(dirty_frames_ptr, expected_dirty_frame_bytes);
    match s.p.mem.import_shared_dirty_snapshot(
        frame_base,
        mapping_generation,
        dirty_pages,
        dirty_frames,
    ) {
        Ok(()) => {
            s.message.clear();
            0
        }
        Err(error) => {
            s.message = format!("shared dirty snapshot import failed: {error:?}").into_bytes();
            4
        }
    }
}

// ── in-place (zero-copy) publication ─────────────────────────────────────────────────────────
//
// When the runtime is linked against an imported shared `WebAssembly.Memory`, its guest frame
// store already lives in memory every CPU worker can address. These exports describe that store
// and write the page table that points straight at it; no frame bytes are copied.

/// Write `[frame store address, bytes in use, bytes allocated, mapping generation]` as four
/// little-endian u32 values. Returns 0, or 1 without a loaded state.
///
/// # Safety
/// `out` must point to four writable, 4-byte-aligned u32 slots.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_frames_info(out: *mut u32) -> i32 {
    let Some(s) = super::state() else { return 1 };
    let (address, used, capacity) = s.p.mem.frame_store_region();
    let (Ok(address), Ok(used), Ok(capacity)) = (
        u32::try_from(address),
        u32::try_from(used),
        u32::try_from(capacity),
    ) else {
        s.message = b"frame store does not fit a 32-bit address space".to_vec();
        return 3;
    };
    *out = address;
    *out.add(1) = used;
    *out.add(2) = capacity;
    *out.add(3) = s.p.mem.shared_mapping_generation();
    0
}

/// Write the sorted page table with `frame_offset` pointing at the live frames (see
/// [`aw_shared_frames_info`]), expressed relative to `header_base`, the address of the ABI header
/// (workers resolve every offset against it). `mapping_capacity_bytes` must hold one 16-byte
/// record per mapped page. Returns the number of records written, or a negative error: -1 without a loaded state,
/// -2 for insufficient capacity, -3 for invalid pointers or addresses.
///
/// # Safety
/// `mappings_ptr` must be 8-byte aligned and writable for `mapping_capacity_bytes`.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_export_mappings_in_place(
    mappings_ptr: *mut u8,
    mapping_capacity_bytes: usize,
    header_base: u32,
) -> i64 {
    let Some(s) = super::state() else { return -1 };
    if mappings_ptr.is_null() || mappings_ptr as usize % MAPPING_ENTRY_ALIGN != 0 {
        s.message = b"invalid in-place mapping output buffer".to_vec();
        return -3;
    }
    let (address, _, _) = s.p.mem.frame_store_region();
    let Some(region_base) = u32::try_from(address)
        .ok()
        .and_then(|address| address.checked_sub(header_base))
    else {
        s.message = b"frame store must lie above the shared ABI header".to_vec();
        return -3;
    };
    let capacity = mapping_capacity_bytes / MAPPING_ENTRY_BYTES;
    let mappings = std::slice::from_raw_parts_mut(mappings_ptr.cast::<MappingEntry>(), capacity);
    match s
        .p
        .mem
        .export_shared_mappings_in_place(region_base, mappings)
    {
        Ok(count) => {
            s.message.clear();
            count as i64
        }
        Err(anywasm_rt::SharedSnapshotError::BufferTooSmall) => {
            s.message = b"in-place mapping output buffer is too small".to_vec();
            -2
        }
        Err(error) => {
            s.message = format!("in-place mapping export failed: {error:?}").into_bytes();
            -3
        }
    }
}

/// Tell the runtime which frames CPU workers wrote in place so translations of executable pages
/// are invalidated. Returns how many of them back executable pages (0 keeps all translations).
///
/// # Safety
/// `frames` must point to `count` readable, 4-byte-aligned u32 frame indices.
#[no_mangle]
pub unsafe extern "C" fn aw_shared_note_worker_writes(frames: *const u32, count: usize) -> u32 {
    let Some(s) = super::state() else { return 0 };
    if frames.is_null() || count == 0 {
        return 0;
    }
    s.p.mem
        .note_shared_worker_writes(std::slice::from_raw_parts(frames, count)) as u32
}
