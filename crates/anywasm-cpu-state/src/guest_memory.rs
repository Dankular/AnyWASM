//! Permission-checked access to guest frames stored in shared linear memory.
//!
//! This is the worker-side backing store primitive. Mapping snapshots are immutable for the
//! lifetime of a view; the coordinator must quiesce workers before replacing a snapshot. Guest
//! bytes are accessed only through atomics so concurrent workers never create Rust data races.

use core::ptr::NonNull;
#[cfg(target_has_atomic = "16")]
use core::sync::atomic::AtomicU16;
use core::sync::atomic::AtomicU32;
#[cfg(target_has_atomic = "64")]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::memory::{Access as CpuAccess, Fault as CpuFault, GuestMemory, MemResult};
use crate::shared::{LayoutError, MappingEntry, Region, SharedHeader, GUEST_PAGE_SIZE};

const PAGE_MASK: u64 = GUEST_PAGE_SIZE as u64 - 1;
const GUEST_ADDRESS_LIMIT: u64 = 1 << 48;

pub const PROT_R: u8 = 1;
pub const PROT_W: u8 = 2;
pub const PROT_X: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    InvalidLayout,
    AddressOverflow,
    NonCanonicalAddress,
    Unmapped,
    Protection,
    UnsupportedAtomicAlignment,
    UnsupportedAtomicWidth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub address: u64,
    pub access: Access,
    pub kind: FaultKind,
}

/// A worker-local view over a shared frame store and an immutable mapping snapshot.
pub struct SharedGuestMemory<'a> {
    base: NonNull<AtomicU8>,
    total_bytes: u32,
    frames: Region,
    mappings: &'a [MappingEntry],
}

impl<'a> SharedGuestMemory<'a> {
    /// Construct a view over the shared Wasm memory.
    ///
    /// # Safety
    ///
    /// `base` must point to the beginning of the same shared linear memory described by `header`,
    /// and remain valid for `available_bytes`. `header` and `mappings` must refer to this memory.
    /// The mapping table must stay immutable while the view exists. Every concurrent guest-frame
    /// access, including coordinator accesses, must use atomic operations; ordinary JS/DataView or
    /// Rust byte-slice access is allowed only while every worker is quiescent.
    pub unsafe fn new(
        base: *mut u8,
        available_bytes: u32,
        header: &'a SharedHeader,
        mappings: &'a [MappingEntry],
    ) -> Result<Self, Fault> {
        let invalid = || Fault {
            address: 0,
            access: Access::Read,
            kind: FaultKind::InvalidLayout,
        };
        if base.is_null()
            || (base as usize) % core::mem::align_of::<SharedHeader>() != 0
            || header as *const SharedHeader as *const u8 != base
        {
            return Err(invalid());
        }
        header
            .validate(available_bytes)
            .map_err(|_: LayoutError| invalid())?;
        header
            .validate_mapping_table(mappings)
            .map_err(|_: LayoutError| invalid())?;
        let expected_map = base.add(header.page_map.offset as usize) as *const MappingEntry;
        if mappings.as_ptr() != expected_map {
            return Err(invalid());
        }
        let base = NonNull::new(base.cast::<AtomicU8>()).ok_or_else(invalid)?;
        Ok(Self {
            base,
            total_bytes: header.total_bytes,
            frames: header.frames,
            mappings,
        })
    }

    pub fn read_bytes(&self, address: u64, output: &mut [u8], access: Access) -> Result<(), Fault> {
        self.validate_range(address, output.len(), access)?;
        let mut guest = address;
        let mut done = 0usize;
        while done < output.len() {
            let entry = self.mapping_for(guest, access)?;
            let page_offset = (guest & PAGE_MASK) as u32;
            let chunk = ((GUEST_PAGE_SIZE - page_offset) as usize).min(output.len() - done);
            let physical = self.frame_offset(entry, page_offset, guest, access)?;
            for index in 0..chunk {
                let byte = unsafe { &*self.base.as_ptr().add(physical as usize + index) };
                output[done + index] = byte.load(Ordering::SeqCst);
            }
            guest += chunk as u64;
            done += chunk;
        }
        Ok(())
    }

    pub fn write_bytes(&self, address: u64, input: &[u8]) -> Result<(), Fault> {
        self.validate_range(address, input.len(), Access::Write)?;
        let mut guest = address;
        let mut done = 0usize;
        while done < input.len() {
            let entry = self.mapping_for(guest, Access::Write)?;
            let page_offset = (guest & PAGE_MASK) as u32;
            let chunk = ((GUEST_PAGE_SIZE - page_offset) as usize).min(input.len() - done);
            let physical = self.frame_offset(entry, page_offset, guest, Access::Write)?;
            for index in 0..chunk {
                let byte = unsafe { &*self.base.as_ptr().add(physical as usize + index) };
                byte.store(input[done + index], Ordering::SeqCst);
            }
            guest += chunk as u64;
            done += chunk;
        }
        Ok(())
    }

    pub fn read_u8(&self, address: u64) -> Result<u8, Fault> {
        let mut bytes = [0; 1];
        self.read_bytes(address, &mut bytes, Access::Read)?;
        Ok(bytes[0])
    }

    pub fn write_u8(&self, address: u64, value: u8) -> Result<(), Fault> {
        self.write_bytes(address, &[value])
    }

    pub fn read_u16(&self, address: u64) -> Result<u16, Fault> {
        if let Some(offset) = self.atomic_offset(address, 2, Access::Read)? {
            #[cfg(target_has_atomic = "16")]
            {
                let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU16>() };
                return Ok(u16::from_le(unsafe { &*ptr }.load(Ordering::SeqCst)));
            }
            #[cfg(not(target_has_atomic = "16"))]
            return Err(self.width_fault(address, Access::Read));
        }
        let mut bytes = [0; 2];
        self.read_bytes(address, &mut bytes, Access::Read)?;
        Ok(u16::from_le_bytes(bytes))
    }

    pub fn write_u16(&self, address: u64, value: u16) -> Result<(), Fault> {
        if let Some(offset) = self.atomic_offset(address, 2, Access::Write)? {
            #[cfg(target_has_atomic = "16")]
            {
                let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU16>() };
                unsafe { &*ptr }.store(value.to_le(), Ordering::SeqCst);
                return Ok(());
            }
            #[cfg(not(target_has_atomic = "16"))]
            return Err(self.width_fault(address, Access::Write));
        }
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn read_u32(&self, address: u64) -> Result<u32, Fault> {
        if let Some(offset) = self.atomic_offset(address, 4, Access::Read)? {
            let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU32>() };
            return Ok(u32::from_le(unsafe { &*ptr }.load(Ordering::SeqCst)));
        }
        let mut bytes = [0; 4];
        self.read_bytes(address, &mut bytes, Access::Read)?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn write_u32(&self, address: u64, value: u32) -> Result<(), Fault> {
        if let Some(offset) = self.atomic_offset(address, 4, Access::Write)? {
            let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU32>() };
            unsafe { &*ptr }.store(value.to_le(), Ordering::SeqCst);
            return Ok(());
        }
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn read_u64(&self, address: u64) -> Result<u64, Fault> {
        if let Some(offset) = self.atomic_offset(address, 8, Access::Read)? {
            #[cfg(target_has_atomic = "64")]
            {
                let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU64>() };
                return Ok(u64::from_le(unsafe { &*ptr }.load(Ordering::SeqCst)));
            }
            #[cfg(not(target_has_atomic = "64"))]
            return Err(self.width_fault(address, Access::Read));
        }
        let mut bytes = [0; 8];
        self.read_bytes(address, &mut bytes, Access::Read)?;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn write_u64(&self, address: u64, value: u64) -> Result<(), Fault> {
        if let Some(offset) = self.atomic_offset(address, 8, Access::Write)? {
            #[cfg(target_has_atomic = "64")]
            {
                let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU64>() };
                unsafe { &*ptr }.store(value.to_le(), Ordering::SeqCst);
                return Ok(());
            }
            #[cfg(not(target_has_atomic = "64"))]
            return Err(self.width_fault(address, Access::Write));
        }
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn read_u128(&self, address: u64) -> Result<u128, Fault> {
        let mut bytes = [0; 16];
        self.read_bytes(address, &mut bytes, Access::Read)?;
        Ok(u128::from_le_bytes(bytes))
    }

    pub fn write_u128(&self, address: u64, value: u128) -> Result<(), Fault> {
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn compare_exchange_u8(
        &self,
        address: u64,
        current: u8,
        new: u8,
    ) -> Result<Result<u8, u8>, Fault> {
        let offset = self.atomic_rmw_offset(address, 1)?;
        let ptr = unsafe {
            self.base
                .as_ptr()
                .add(offset.ok_or_else(|| self.alignment_fault(address, 1))? as usize)
        };
        Ok(unsafe { &*ptr }.compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst))
    }

    pub fn compare_exchange_u16(
        &self,
        address: u64,
        current: u16,
        new: u16,
    ) -> Result<Result<u16, u16>, Fault> {
        let Some(offset) = self.atomic_rmw_offset(address, 2)? else {
            return Err(self.alignment_fault(address, 2));
        };
        #[cfg(target_has_atomic = "16")]
        {
            let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU16>() };
            return Ok(unsafe { &*ptr }
                .compare_exchange(
                    current.to_le(),
                    new.to_le(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .map(u16::from_le)
                .map_err(u16::from_le));
        }
        #[allow(unreachable_code)]
        Err(Fault {
            address,
            access: Access::Write,
            kind: FaultKind::UnsupportedAtomicWidth,
        })
    }

    pub fn compare_exchange_u32(
        &self,
        address: u64,
        current: u32,
        new: u32,
    ) -> Result<Result<u32, u32>, Fault> {
        let Some(offset) = self.atomic_rmw_offset(address, 4)? else {
            return Err(self.alignment_fault(address, 4));
        };
        let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU32>() };
        Ok(unsafe { &*ptr }
            .compare_exchange(
                current.to_le(),
                new.to_le(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map(u32::from_le)
            .map_err(u32::from_le))
    }

    pub fn compare_exchange_u64(
        &self,
        address: u64,
        current: u64,
        new: u64,
    ) -> Result<Result<u64, u64>, Fault> {
        let Some(offset) = self.atomic_rmw_offset(address, 8)? else {
            return Err(self.alignment_fault(address, 8));
        };
        #[cfg(target_has_atomic = "64")]
        {
            let ptr = unsafe { self.base.as_ptr().add(offset as usize).cast::<AtomicU64>() };
            return Ok(unsafe { &*ptr }
                .compare_exchange(
                    current.to_le(),
                    new.to_le(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .map(u64::from_le)
                .map_err(u64::from_le));
        }
        #[allow(unreachable_code)]
        Err(Fault {
            address,
            access: Access::Write,
            kind: FaultKind::UnsupportedAtomicWidth,
        })
    }

    fn atomic_offset(
        &self,
        address: u64,
        width: u32,
        access: Access,
    ) -> Result<Option<u32>, Fault> {
        self.validate_range(address, width as usize, access)?;
        let page_offset = (address & PAGE_MASK) as u32;
        if address % width as u64 != 0 || page_offset + width > GUEST_PAGE_SIZE {
            return Ok(None);
        }
        let entry = self.mapping_for(address, access)?;
        self.frame_offset(entry, page_offset, address, access)
            .map(Some)
    }

    fn atomic_rmw_offset(&self, address: u64, width: u32) -> Result<Option<u32>, Fault> {
        self.validate_range(address, width as usize, Access::Read)
            .map_err(|mut fault| {
                fault.access = Access::Write;
                fault
            })?;
        self.atomic_offset(address, width, Access::Write)
    }

    fn validate_range(&self, address: u64, length: usize, access: Access) -> Result<(), Fault> {
        if length == 0 {
            return Ok(());
        }
        let length = u64::try_from(length).map_err(|_| Fault {
            address,
            access,
            kind: FaultKind::AddressOverflow,
        })?;
        let end = address.checked_add(length).ok_or(Fault {
            address,
            access,
            kind: FaultKind::AddressOverflow,
        })?;
        if end > GUEST_ADDRESS_LIMIT {
            return Err(Fault {
                address,
                access,
                kind: FaultKind::NonCanonicalAddress,
            });
        }
        let mut cursor = address;
        while cursor < end {
            let entry = self.mapping_for(cursor, access)?;
            let page_offset = (cursor & PAGE_MASK) as u32;
            let chunk = ((GUEST_PAGE_SIZE - page_offset) as u64).min(end - cursor) as u32;
            self.frame_offset(entry, page_offset, cursor, access)?;
            cursor += chunk as u64;
        }
        Ok(())
    }

    fn mapping_for(&self, address: u64, access: Access) -> Result<&MappingEntry, Fault> {
        let page = address >> 12;
        let entry = self
            .mappings
            .binary_search_by_key(&page, |entry| entry.guest_page)
            .map(|index| &self.mappings[index])
            .map_err(|_| Fault {
                address,
                access,
                kind: FaultKind::Unmapped,
            })?;
        let permitted = match access {
            Access::Read => entry.protection & (PROT_R | PROT_X) != 0,
            Access::Write => entry.protection & PROT_W != 0,
            Access::Execute => entry.protection & PROT_X != 0,
        };
        if !permitted {
            return Err(Fault {
                address,
                access,
                kind: FaultKind::Protection,
            });
        }
        Ok(entry)
    }

    fn frame_offset(
        &self,
        entry: &MappingEntry,
        page_offset: u32,
        address: u64,
        access: Access,
    ) -> Result<u32, Fault> {
        let offset = entry.frame_offset.checked_add(page_offset).ok_or(Fault {
            address,
            access,
            kind: FaultKind::InvalidLayout,
        })?;
        let frames_end = self
            .frames
            .offset
            .checked_add(self.frames.length)
            .ok_or(Fault {
                address,
                access,
                kind: FaultKind::InvalidLayout,
            })?;
        if offset < self.frames.offset || offset >= frames_end || offset >= self.total_bytes {
            return Err(Fault {
                address,
                access,
                kind: FaultKind::InvalidLayout,
            });
        }
        Ok(offset)
    }

    fn alignment_fault(&self, address: u64, width: u32) -> Fault {
        Fault {
            address,
            access: Access::Write,
            kind: if address % width as u64 == 0
                && (address & PAGE_MASK) as u32 + width <= GUEST_PAGE_SIZE
            {
                FaultKind::UnsupportedAtomicWidth
            } else {
                FaultKind::UnsupportedAtomicAlignment
            },
        }
    }

    #[cfg(any(not(target_has_atomic = "16"), not(target_has_atomic = "64")))]
    fn width_fault(&self, address: u64, access: Access) -> Fault {
        Fault {
            address,
            access,
            kind: FaultKind::UnsupportedAtomicWidth,
        }
    }
}

impl GuestMemory for SharedGuestMemory<'_> {
    fn read_u8(&self, address: u64) -> MemResult<u8> {
        SharedGuestMemory::read_u8(self, address).map_err(to_cpu_fault)
    }
    fn read_u16(&self, address: u64) -> MemResult<u16> {
        SharedGuestMemory::read_u16(self, address).map_err(to_cpu_fault)
    }
    fn read_u32(&self, address: u64) -> MemResult<u32> {
        SharedGuestMemory::read_u32(self, address).map_err(to_cpu_fault)
    }
    fn read_u64(&self, address: u64) -> MemResult<u64> {
        SharedGuestMemory::read_u64(self, address).map_err(to_cpu_fault)
    }
    fn read_u128(&self, address: u64) -> MemResult<u128> {
        SharedGuestMemory::read_u128(self, address).map_err(to_cpu_fault)
    }
    fn write_u8(&mut self, address: u64, value: u8) -> MemResult<()> {
        SharedGuestMemory::write_u8(self, address, value).map_err(to_cpu_fault)
    }
    fn write_u16(&mut self, address: u64, value: u16) -> MemResult<()> {
        SharedGuestMemory::write_u16(self, address, value).map_err(to_cpu_fault)
    }
    fn write_u32(&mut self, address: u64, value: u32) -> MemResult<()> {
        SharedGuestMemory::write_u32(self, address, value).map_err(to_cpu_fault)
    }
    fn write_u64(&mut self, address: u64, value: u64) -> MemResult<()> {
        SharedGuestMemory::write_u64(self, address, value).map_err(to_cpu_fault)
    }
    fn write_u128(&mut self, address: u64, value: u128) -> MemResult<()> {
        SharedGuestMemory::write_u128(self, address, value).map_err(to_cpu_fault)
    }
    fn compare_exchange_u8(
        &mut self,
        address: u64,
        current: u8,
        new: u8,
    ) -> MemResult<Result<u8, u8>> {
        SharedGuestMemory::compare_exchange_u8(self, address, current, new).map_err(to_cpu_fault)
    }
    fn compare_exchange_u16(
        &mut self,
        address: u64,
        current: u16,
        new: u16,
    ) -> MemResult<Result<u16, u16>> {
        SharedGuestMemory::compare_exchange_u16(self, address, current, new).map_err(to_cpu_fault)
    }
    fn compare_exchange_u32(
        &mut self,
        address: u64,
        current: u32,
        new: u32,
    ) -> MemResult<Result<u32, u32>> {
        SharedGuestMemory::compare_exchange_u32(self, address, current, new).map_err(to_cpu_fault)
    }
    fn compare_exchange_u64(
        &mut self,
        address: u64,
        current: u64,
        new: u64,
    ) -> MemResult<Result<u64, u64>> {
        SharedGuestMemory::compare_exchange_u64(self, address, current, new).map_err(to_cpu_fault)
    }
    fn compare_exchange_u128(
        &mut self,
        address: u64,
        _current: u128,
        _new: u128,
    ) -> MemResult<Result<u128, u128>> {
        Err(to_cpu_fault(Fault {
            address,
            access: Access::Write,
            kind: FaultKind::UnsupportedAtomicWidth,
        }))
    }
    fn read_into(&self, address: u64, output: &mut [u8], access: CpuAccess) -> MemResult<()> {
        SharedGuestMemory::read_bytes(self, address, output, from_cpu_access(access))
            .map_err(to_cpu_fault)
    }
    fn write_from(&mut self, address: u64, input: &[u8]) -> MemResult<()> {
        SharedGuestMemory::write_bytes(self, address, input).map_err(to_cpu_fault)
    }
    fn fetch(&self, address: u64, output: &mut [u8; 15]) -> MemResult<usize> {
        let mut fetched = 0;
        while fetched < output.len() {
            match self.read_bytes(
                address.wrapping_add(fetched as u64),
                &mut output[fetched..fetched + 1],
                Access::Execute,
            ) {
                Ok(()) => fetched += 1,
                Err(error) if fetched == 0 => return Err(to_cpu_fault(error)),
                Err(_) => break,
            }
        }
        Ok(fetched)
    }
}

fn from_cpu_access(access: CpuAccess) -> Access {
    match access {
        CpuAccess::Read => Access::Read,
        CpuAccess::Write => Access::Write,
        CpuAccess::Exec => Access::Execute,
    }
}

fn to_cpu_fault(fault: Fault) -> CpuFault {
    CpuFault {
        addr: fault.address,
        access: match fault.access {
            Access::Read => CpuAccess::Read,
            Access::Write => CpuAccess::Write,
            Access::Execute => CpuAccess::Exec,
        },
        protection: fault.kind == FaultKind::Protection,
    }
}
