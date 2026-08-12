use core::{fmt::Display, ops::Range};

use num_align::NumAlign;

/// One physical-memory region discovered or reserved during boot.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct MemoryDescriptor {
    /// Physical start address of the region.
    pub physical_start: usize,
    /// Length of the region in bytes.
    pub size_in_bytes: usize,
    /// Boot-time classification of the region.
    pub memory_type: MemoryType,
}

impl MemoryDescriptor {
    /// Creates a descriptor from an already validated physical range.
    pub fn new_with_range(range: Range<usize>, memory_type: MemoryType) -> Self {
        Self {
            physical_start: range.start,
            size_in_bytes: range.end - range.start,
            memory_type,
        }
    }

    /// Creates a descriptor covering `range`, rounded out to `align`.
    pub fn new_with_range_aligned(
        range: Range<usize>,
        memory_type: MemoryType,
        align: usize,
    ) -> Self {
        let start = range.start.align_down(align);
        let end = range.end.align_up(align);
        Self {
            physical_start: start,
            size_in_bytes: end - start,
            memory_type,
        }
    }

    /// Creates an aligned descriptor covering the supplied physical span.
    pub fn new_aligned(
        physical_start: usize,
        size_in_bytes: usize,
        memory_type: MemoryType,
        align: usize,
    ) -> Self {
        let start = physical_start.align_down(align);
        let end = (physical_start + size_in_bytes).align_up(align);
        Self {
            physical_start: start,
            size_in_bytes: end - start,
            memory_type,
        }
    }
}

impl MemoryDescriptor {
    /// Returns the exclusive end address, or `None` when it overflows.
    pub(crate) fn checked_end(&self) -> Option<usize> {
        self.physical_start.checked_add(self.size_in_bytes)
    }
}

/// Boot-time use assigned to a physical-memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryType {
    /// Memory available for the runtime allocator.
    #[default]
    Free,
    /// Firmware-described RAM whose final use is not assigned yet.
    Ram,
    /// The loaded kernel image.
    KImage,
    /// Memory unavailable to the runtime allocator.
    Reserved,
    /// Device register space.
    Mmio,
    /// Runtime CPU-local storage reserved during boot.
    PerCpuData,
}

impl Display for MemoryType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let label = match self {
            Self::Free => "Free  ",
            Self::Ram => "RAM   ",
            Self::KImage => "KImg  ",
            Self::Reserved => "Rsv   ",
            Self::Mmio => "MMIO  ",
            Self::PerCpuData => "PerCPU",
        };
        f.write_str(label)
    }
}

/// Architecture page-table register state used during boot handoff.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PageTableInfo {
    /// Address-space identifier encoded by the architecture register.
    pub asid: usize,
    /// Physical address of the root page table.
    pub addr: usize,
}

impl PageTableInfo {
    /// Returns an empty page-table register state.
    pub const fn zero() -> Self {
        Self { asid: 0, addr: 0 }
    }
}
