//! Shared-memory layout and the coordinator/worker dispatch handoff.
//!
//! The module is available only on targets with 32-bit atomics. For `wasm32-unknown-unknown`,
//! build the worker artifact with `+atomics`; the ordinary coordinator artifact keeps its current
//! private memory and does not use this ABI.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::Cpu;

pub const ABI_MAGIC: u32 = u32::from_le_bytes(*b"AWSH");
pub const ABI_VERSION: u32 = 2;
pub const GUEST_PAGE_SIZE: u32 = 4096;

pub const SLOT_IDLE: u32 = 0;
pub const SLOT_WRITING: u32 = 1;
pub const SLOT_READY: u32 = 2;
pub const SLOT_RUNNING: u32 = 3;
pub const SLOT_QUANTUM: u32 = 4;
pub const SLOT_HLE: u32 = 5;
pub const SLOT_FAULT: u32 = 6;
pub const SLOT_EXITED: u32 = 7;
pub const SLOT_FAILED: u32 = 8;
pub const SLOT_DISABLED: u32 = 9;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Region {
    pub offset: u32,
    pub length: u32,
}

/// Fixed header at byte offset zero in the shared memory object.
#[repr(C, align(64))]
pub struct SharedHeader {
    pub magic: u32,
    pub version: u32,
    pub header_bytes: u32,
    pub total_bytes: u32,
    pub worker_count: u32,
    pub context_count: u32,
    pub slots: Region,
    pub contexts: Region,
    pub page_map: Region,
    pub frames: Region,
    pub page_map_count: AtomicU32,
    pub mapping_generation: AtomicU32,
    pub code_generation: AtomicU32,
    pub dispatch_generation: AtomicU32,
    pub flags: AtomicU32,
    /// One bit per packed guest page; workers set a bit after writing that frame.
    pub dirty_map: Region,
}

impl SharedHeader {
    pub const fn new(
        total_bytes: u32,
        worker_count: u32,
        context_count: u32,
        slots: Region,
        contexts: Region,
        page_map: Region,
        dirty_map: Region,
        frames: Region,
    ) -> Self {
        Self {
            magic: ABI_MAGIC,
            version: ABI_VERSION,
            header_bytes: core::mem::size_of::<Self>() as u32,
            total_bytes,
            worker_count,
            context_count,
            slots,
            contexts,
            page_map,
            dirty_map,
            frames,
            page_map_count: AtomicU32::new(0),
            mapping_generation: AtomicU32::new(0),
            code_generation: AtomicU32::new(0),
            dispatch_generation: AtomicU32::new(0),
            flags: AtomicU32::new(0),
        }
    }

    /// Validate every region before a worker forms pointers into shared memory.
    pub fn validate(&self, available_bytes: u32) -> Result<(), LayoutError> {
        if self.magic != ABI_MAGIC {
            return Err(LayoutError::BadMagic);
        }
        if self.version != ABI_VERSION {
            return Err(LayoutError::BadVersion);
        }
        if self.flags.load(Ordering::Relaxed) != 0 {
            return Err(LayoutError::UnsupportedFlags);
        }
        if self.header_bytes != core::mem::size_of::<Self>() as u32 {
            return Err(LayoutError::BadHeaderSize);
        }
        if self.total_bytes > available_bytes || self.total_bytes < self.header_bytes {
            return Err(LayoutError::OutOfBounds);
        }
        if self.worker_count == 0 || self.context_count == 0 {
            return Err(LayoutError::EmptyRegion);
        }
        let slots_required = self
            .worker_count
            .checked_mul(core::mem::size_of::<WorkerSlot>() as u32)
            .ok_or(LayoutError::RegionTooSmall)?;
        let contexts_required = self
            .context_count
            .checked_mul(core::mem::size_of::<Cpu>() as u32)
            .ok_or(LayoutError::RegionTooSmall)?;
        if self.slots.length < slots_required || self.contexts.length < contexts_required {
            return Err(LayoutError::RegionTooSmall);
        }
        if self.page_map.length % core::mem::size_of::<MappingEntry>() as u32 != 0
            || self.frames.length == 0
            || self.frames.length % GUEST_PAGE_SIZE != 0
        {
            return Err(LayoutError::BadRegionSize);
        }
        let map_capacity = self.page_map.length / core::mem::size_of::<MappingEntry>() as u32;
        if self.page_map_count.load(Ordering::Acquire) > map_capacity {
            return Err(LayoutError::RegionTooSmall);
        }
        let dirty_map_required = map_capacity.div_ceil(8);
        if self.dirty_map.length < dirty_map_required {
            return Err(LayoutError::RegionTooSmall);
        }

        let regions = [
            Region {
                offset: 0,
                length: self.header_bytes,
            },
            self.slots,
            self.contexts,
            self.page_map,
            self.dirty_map,
            self.frames,
        ];
        let alignments = [64, 64, 16, 8, 1, GUEST_PAGE_SIZE];
        for (index, region) in regions.iter().enumerate() {
            if region.length == 0 && index == 3 {
                continue;
            }
            let end = region
                .offset
                .checked_add(region.length)
                .ok_or(LayoutError::OutOfBounds)?;
            if region.offset % alignments[index] != 0 || end > self.total_bytes {
                return Err(LayoutError::OutOfBounds);
            }
            if index != 0 && region.offset < self.header_bytes {
                return Err(LayoutError::OverlappingRegions);
            }
            for other in regions.iter().skip(index + 1) {
                if region.length == 0 || other.length == 0 {
                    continue;
                }
                let region_end = region.offset + region.length;
                let other_end = other
                    .offset
                    .checked_add(other.length)
                    .ok_or(LayoutError::OutOfBounds)?;
                if region.offset < other_end && other.offset < region_end {
                    return Err(LayoutError::OverlappingRegions);
                }
            }
        }
        Ok(())
    }

    pub fn worker_slot_offset(&self, index: u32) -> Result<u32, LayoutError> {
        if index >= self.worker_count {
            return Err(LayoutError::OutOfBounds);
        }
        let displacement = index
            .checked_mul(core::mem::size_of::<WorkerSlot>() as u32)
            .ok_or(LayoutError::OutOfBounds)?;
        let offset = self
            .slots
            .offset
            .checked_add(displacement)
            .ok_or(LayoutError::OutOfBounds)?;
        let slot_end = self
            .slots
            .offset
            .checked_add(self.slots.length)
            .ok_or(LayoutError::OutOfBounds)?;
        let end = offset
            .checked_add(core::mem::size_of::<WorkerSlot>() as u32)
            .ok_or(LayoutError::OutOfBounds)?;
        if end > slot_end {
            return Err(LayoutError::OutOfBounds);
        }
        Ok(offset)
    }

    pub fn validate_context_offset(&self, offset: u32) -> Result<(), LayoutError> {
        let context_bytes = core::mem::size_of::<Cpu>() as u32;
        let relative = offset
            .checked_sub(self.contexts.offset)
            .ok_or(LayoutError::OutOfBounds)?;
        let context_index = relative / context_bytes;
        let end = offset
            .checked_add(context_bytes)
            .ok_or(LayoutError::OutOfBounds)?;
        let contexts_end = self
            .contexts
            .offset
            .checked_add(self.contexts.length)
            .ok_or(LayoutError::OutOfBounds)?;
        if offset % core::mem::align_of::<Cpu>() as u32 != 0
            || relative % context_bytes != 0
            || context_index >= self.context_count
            || end > contexts_end
        {
            return Err(LayoutError::OutOfBounds);
        }
        Ok(())
    }

    pub fn validate_mapping_entry(&self, entry: MappingEntry) -> Result<(), LayoutError> {
        let frames_end = self
            .frames
            .offset
            .checked_add(self.frames.length)
            .ok_or(LayoutError::OutOfBounds)?;
        let frame_end = entry
            .frame_offset
            .checked_add(GUEST_PAGE_SIZE)
            .ok_or(LayoutError::OutOfBounds)?;
        if entry.guest_page >= (1u64 << 36)
            || entry.frame_offset % GUEST_PAGE_SIZE != 0
            || entry.protection & !0x07 != 0
            || entry.reserved != [0; 3]
            || entry.frame_offset < self.frames.offset
            || frame_end > frames_end
        {
            return Err(LayoutError::BadMappingEntry);
        }
        Ok(())
    }

    /// Validate a snapshot after its published count has been acquired from the header.
    pub fn validate_mapping_table(&self, entries: &[MappingEntry]) -> Result<(), LayoutError> {
        let expected = self.page_map_count.load(Ordering::Acquire) as usize;
        if entries.len() != expected {
            return Err(LayoutError::BadRegionSize);
        }
        let mut previous = None;
        for entry in entries {
            self.validate_mapping_entry(*entry)?;
            if previous.is_some_and(|page| page >= entry.guest_page) {
                return Err(LayoutError::BadMappingEntry);
            }
            previous = Some(entry.guest_page);
        }
        Ok(())
    }
}

/// One sorted guest-page mapping in the worker's published map snapshot.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MappingEntry {
    /// Guest virtual page number (`guest_address >> 12`).
    pub guest_page: u64,
    /// Byte offset in the shared memory object where this page's 4096 bytes begin.
    pub frame_offset: u32,
    /// `PROT_R | PROT_W | PROT_X` bits from the CPU memory model.
    pub protection: u8,
    pub reserved: [u8; 3],
}

/// One cache-line-sized worker-control record. All concurrently exchanged values are atomic;
/// context bytes are owned by the worker between READY->RUNNING and a completion publication.
#[repr(C, align(64))]
pub struct WorkerSlot {
    state: AtomicU32,
    sequence: AtomicU32,
    context_offset: AtomicU32,
    thread_id: AtomicU32,
    budget_low: AtomicU32,
    budget_high: AtomicU32,
    mapping_generation: AtomicU32,
    code_generation: AtomicU32,
    dispatch_generation: AtomicU32,
    retired_low: AtomicU32,
    retired_high: AtomicU32,
    stop_reason: AtomicU32,
    payload_address_low: AtomicU32,
    payload_address_high: AtomicU32,
    payload_value_low: AtomicU32,
    payload_value_high: AtomicU32,
}

impl WorkerSlot {
    pub const fn new() -> Self {
        Self {
            state: AtomicU32::new(SLOT_IDLE),
            sequence: AtomicU32::new(0),
            context_offset: AtomicU32::new(0),
            thread_id: AtomicU32::new(0),
            budget_low: AtomicU32::new(0),
            budget_high: AtomicU32::new(0),
            mapping_generation: AtomicU32::new(0),
            code_generation: AtomicU32::new(0),
            dispatch_generation: AtomicU32::new(0),
            retired_low: AtomicU32::new(0),
            retired_high: AtomicU32::new(0),
            stop_reason: AtomicU32::new(0),
            payload_address_low: AtomicU32::new(0),
            payload_address_high: AtomicU32::new(0),
            payload_value_low: AtomicU32::new(0),
            payload_value_high: AtomicU32::new(0),
        }
    }

    pub fn state(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    /// Publish one dispatch. The coordinator exclusively owns the slot while it is IDLE/WRITING.
    pub fn publish(&self, dispatch: Dispatch) -> Result<u32, u32> {
        self.state.compare_exchange(
            SLOT_IDLE,
            SLOT_WRITING,
            Ordering::Acquire,
            Ordering::Relaxed,
        )?;
        let sequence = self
            .sequence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.context_offset
            .store(dispatch.context_offset, Ordering::Relaxed);
        self.thread_id.store(dispatch.thread_id, Ordering::Relaxed);
        self.budget_low
            .store(dispatch.instruction_budget as u32, Ordering::Relaxed);
        self.budget_high.store(
            (dispatch.instruction_budget >> 32) as u32,
            Ordering::Relaxed,
        );
        self.mapping_generation
            .store(dispatch.mapping_generation, Ordering::Relaxed);
        self.code_generation
            .store(dispatch.code_generation, Ordering::Relaxed);
        self.dispatch_generation
            .store(dispatch.dispatch_generation, Ordering::Relaxed);
        self.retired_low.store(0, Ordering::Relaxed);
        self.retired_high.store(0, Ordering::Relaxed);
        self.stop_reason.store(0, Ordering::Relaxed);
        self.payload_address_low.store(0, Ordering::Relaxed);
        self.payload_address_high.store(0, Ordering::Relaxed);
        self.payload_value_low.store(0, Ordering::Relaxed);
        self.payload_value_high.store(0, Ordering::Relaxed);
        self.state.store(SLOT_READY, Ordering::Release);
        Ok(sequence)
    }

    /// Claim a published dispatch. The acquire transition makes its payload visible to the worker.
    pub fn try_claim(&self) -> Option<ClaimedDispatch> {
        self.state
            .compare_exchange(
                SLOT_READY,
                SLOT_RUNNING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()?;
        Some(ClaimedDispatch {
            sequence: self.sequence.load(Ordering::Relaxed),
            dispatch: Dispatch {
                context_offset: self.context_offset.load(Ordering::Relaxed),
                thread_id: self.thread_id.load(Ordering::Relaxed),
                instruction_budget: join_u64(
                    self.budget_low.load(Ordering::Relaxed),
                    self.budget_high.load(Ordering::Relaxed),
                ),
                mapping_generation: self.mapping_generation.load(Ordering::Relaxed),
                code_generation: self.code_generation.load(Ordering::Relaxed),
                dispatch_generation: self.dispatch_generation.load(Ordering::Relaxed),
            },
        })
    }

    /// Publish the worker's completed slice. Release makes context writes visible to the coordinator.
    pub fn complete(&self, completion: Completion) -> bool {
        if self.sequence.load(Ordering::Acquire) != completion.sequence
            || self
                .state
                .compare_exchange(
                    SLOT_RUNNING,
                    SLOT_WRITING,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_err()
        {
            return false;
        }
        self.retired_low
            .store(completion.retired_instructions as u32, Ordering::Relaxed);
        self.retired_high.store(
            (completion.retired_instructions >> 32) as u32,
            Ordering::Relaxed,
        );
        self.stop_reason
            .store(completion.stop_reason, Ordering::Relaxed);
        self.payload_address_low
            .store(completion.payload_address as u32, Ordering::Relaxed);
        self.payload_address_high
            .store((completion.payload_address >> 32) as u32, Ordering::Relaxed);
        self.payload_value_low
            .store(completion.payload_value as u32, Ordering::Relaxed);
        self.payload_value_high
            .store((completion.payload_value >> 32) as u32, Ordering::Relaxed);
        self.state.store(completion.state as u32, Ordering::Release);
        true
    }

    /// Read one completed slice and release the slot, disabling it after worker failure.
    pub fn take_completion(&self) -> Option<Completion> {
        let state = self.state.load(Ordering::Acquire);
        if !matches!(
            state,
            SLOT_QUANTUM | SLOT_HLE | SLOT_FAULT | SLOT_EXITED | SLOT_FAILED
        ) {
            return None;
        }
        let completion = Completion {
            sequence: self.sequence.load(Ordering::Relaxed),
            state: CompletionState::from_raw(state)?,
            retired_instructions: join_u64(
                self.retired_low.load(Ordering::Relaxed),
                self.retired_high.load(Ordering::Relaxed),
            ),
            stop_reason: self.stop_reason.load(Ordering::Relaxed),
            payload_address: join_u64(
                self.payload_address_low.load(Ordering::Relaxed),
                self.payload_address_high.load(Ordering::Relaxed),
            ),
            payload_value: join_u64(
                self.payload_value_low.load(Ordering::Relaxed),
                self.payload_value_high.load(Ordering::Relaxed),
            ),
        };
        let next_state = if state == SLOT_FAILED {
            SLOT_DISABLED
        } else {
            SLOT_IDLE
        };
        self.state
            .compare_exchange(state, next_state, Ordering::Release, Ordering::Relaxed)
            .ok()?;
        Some(completion)
    }
}

impl Default for WorkerSlot {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dispatch {
    pub context_offset: u32,
    pub thread_id: u32,
    pub instruction_budget: u64,
    pub mapping_generation: u32,
    pub code_generation: u32,
    pub dispatch_generation: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaimedDispatch {
    pub sequence: u32,
    pub dispatch: Dispatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum CompletionState {
    Quantum = SLOT_QUANTUM,
    Hle = SLOT_HLE,
    Fault = SLOT_FAULT,
    Exited = SLOT_EXITED,
    Failed = SLOT_FAILED,
}

impl CompletionState {
    fn from_raw(value: u32) -> Option<Self> {
        match value {
            SLOT_QUANTUM => Some(Self::Quantum),
            SLOT_HLE => Some(Self::Hle),
            SLOT_FAULT => Some(Self::Fault),
            SLOT_EXITED => Some(Self::Exited),
            SLOT_FAILED => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    pub sequence: u32,
    pub state: CompletionState,
    pub retired_instructions: u64,
    pub stop_reason: u32,
    pub payload_address: u64,
    pub payload_value: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    BadMagic,
    BadVersion,
    BadHeaderSize,
    OutOfBounds,
    EmptyRegion,
    RegionTooSmall,
    BadRegionSize,
    BadMappingEntry,
    UnsupportedFlags,
    OverlappingRegions,
}

#[inline]
const fn join_u64(low: u32, high: u32) -> u64 {
    (low as u64) | ((high as u64) << 32)
}

const _: [(); 128] = [(); core::mem::size_of::<SharedHeader>()];
const _: [(); 56] = [(); core::mem::offset_of!(SharedHeader, page_map_count)];
const _: [(); 60] = [(); core::mem::offset_of!(SharedHeader, mapping_generation)];
const _: [(); 76] = [(); core::mem::offset_of!(SharedHeader, dirty_map)];
const _: [(); 64] = [(); core::mem::size_of::<WorkerSlot>()];
const _: [(); 64] = [(); core::mem::align_of::<WorkerSlot>()];
const _: [(); 16] = [(); core::mem::size_of::<MappingEntry>()];
const _: [(); 0] = [(); core::mem::offset_of!(WorkerSlot, state)];
const _: [(); 4] = [(); core::mem::offset_of!(WorkerSlot, sequence)];
const _: [(); 60] = [(); core::mem::offset_of!(WorkerSlot, payload_value_high)];
