//! CPU cache maintenance.

pub use crate::arch::current::cache::*;

/// A checked byte range for CPU cache maintenance.
/// Construction validates arithmetic, not mapping or memory ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ByteRange {
    start: usize,
    last: Option<usize>,
}

/// The requested cache byte range wraps the CPU address space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheRangeOverflow;

impl core::fmt::Display for CacheRangeOverflow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("cache range wraps the address space")
    }
}
impl core::error::Error for CacheRangeOverflow {}

impl ByteRange {
    /// Checks the inclusive endpoint. A zero-byte request touches no cache line.
    const fn new(start: usize, bytes: usize) -> Result<Self, CacheRangeOverflow> {
        let last = if bytes == 0 {
            None
        } else {
            match start.checked_add(bytes - 1) {
                Some(last) => Some(last),
                None => return Err(CacheRangeOverflow),
            }
        };
        Ok(Self { start, last })
    }

    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        all(target_arch = "riscv64", feature = "riscv-thead-mae")
    ))]
    pub(crate) fn for_each_line(self, line_size: usize, mut operation: impl FnMut(usize)) {
        let Some(last) = self.last else { return };
        let mask = line_size - 1;
        let last_line = last & !mask;
        let mut line = self.start & !mask;
        loop {
            operation(line);
            if line == last_line {
                break;
            }
            line += line_size;
        }
    }
}

/// A checked virtual byte range. Construction validates arithmetic, not mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheRange(ByteRange);
impl CacheRange {
    /// Checks the inclusive endpoint; empty ranges perform no maintenance.
    pub const fn new(start: crate::VirtAddr, bytes: usize) -> Result<Self, CacheRangeOverflow> {
        match ByteRange::new(start.as_usize(), bytes) {
            Ok(range) => Ok(Self(range)),
            Err(error) => Err(error),
        }
    }
    /// Returns the first requested virtual byte.
    pub const fn start(self) -> crate::VirtAddr {
        crate::VirtAddr::from_usize(self.0.start)
    }
    /// Reports a zero-length request.
    pub const fn is_empty(self) -> bool {
        self.0.last.is_none()
    }
    #[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
    pub(crate) fn for_each_line(self, size: usize, operation: impl FnMut(usize)) {
        self.0.for_each_line(size, operation);
    }
}

/// A checked physical byte range for physical-address cache instructions.
#[cfg(all(target_arch = "riscv64", feature = "riscv-thead-mae"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalCacheRange(ByteRange);
#[cfg(all(target_arch = "riscv64", feature = "riscv-thead-mae"))]
impl PhysicalCacheRange {
    /// Checks physical endpoint arithmetic without converting a virtual alias.
    pub const fn new(start: crate::PhysAddr, bytes: usize) -> Result<Self, CacheRangeOverflow> {
        match ByteRange::new(start.as_usize(), bytes) {
            Ok(range) => Ok(Self(range)),
            Err(error) => Err(error),
        }
    }
    /// Returns the first requested physical byte.
    pub const fn start(self) -> crate::PhysAddr {
        crate::PhysAddr::from_usize(self.0.start)
    }
    /// Reports a zero-length request.
    pub const fn is_empty(self) -> bool {
        self.0.last.is_none()
    }
    pub(crate) fn for_each_line(self, size: usize, operation: impl FnMut(usize)) {
        self.0.for_each_line(size, operation);
    }
}

/// Completes local translation and instruction synchronization for modified text.
/// The owner must first publish the modified bytes using
/// `clean_dcache_range_to_pou`, and coordinate any other CPUs executing this text.
pub fn sync_kernel_text(start: crate::VirtAddr, size: usize) {
    crate::mmu::flush_tlb_range(start, size);
    flush_icache_all();
}
