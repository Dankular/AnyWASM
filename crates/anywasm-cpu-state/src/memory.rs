//! Allocation-free guest memory interface used by the interpreter and CPU workers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Exec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub addr: u64,
    pub access: Access,
    pub protection: bool,
}

pub type MemResult<T> = Result<T, Fault>;

/// The executor depends on this small interface instead of a process-owned memory manager.
/// Shared-memory workers and the ordinary paged memory backend implement the same operations.
pub trait GuestMemory {
    fn read_u8(&self, address: u64) -> MemResult<u8>;
    fn read_u16(&self, address: u64) -> MemResult<u16>;
    fn read_u32(&self, address: u64) -> MemResult<u32>;
    fn read_u64(&self, address: u64) -> MemResult<u64>;
    fn read_u128(&self, address: u64) -> MemResult<u128>;

    fn write_u8(&mut self, address: u64, value: u8) -> MemResult<()>;
    fn write_u16(&mut self, address: u64, value: u16) -> MemResult<()>;
    fn write_u32(&mut self, address: u64, value: u32) -> MemResult<()>;
    fn write_u64(&mut self, address: u64, value: u64) -> MemResult<()>;
    fn write_u128(&mut self, address: u64, value: u128) -> MemResult<()>;

    fn read_into(&self, address: u64, output: &mut [u8], access: Access) -> MemResult<()>;
    fn write_from(&mut self, address: u64, input: &[u8]) -> MemResult<()>;
    fn fetch(&self, address: u64, output: &mut [u8; 15]) -> MemResult<usize>;

    /// Compare and replace one scalar. Backends with concurrent workers must override this with
    /// a single atomic operation; the default is suitable for the serialized interpreter.
    fn compare_exchange_u64(
        &mut self,
        address: u64,
        current: u64,
        new: u64,
    ) -> MemResult<Result<u64, u64>> {
        let observed = self.read_u64(address)?;
        if observed == current {
            self.write_u64(address, new)?;
        }
        Ok(if observed == current {
            Ok(observed)
        } else {
            Err(observed)
        })
    }
}
