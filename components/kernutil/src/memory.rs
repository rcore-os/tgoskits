use core::fmt::{Debug, Display};

use num_align::NumAlign;

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct MemoryDescriptor {
    pub physical_start: usize,
    pub size_in_bytes: usize,
    pub memory_type: MemoryType,
}

impl MemoryDescriptor {
    pub fn new_with_range(range: core::ops::Range<usize>, memory_type: MemoryType) -> Self {
        MemoryDescriptor {
            physical_start: range.start,
            size_in_bytes: range.end - range.start,
            memory_type,
        }
    }

    pub fn new_with_range_aligned(
        range: core::ops::Range<usize>,
        memory_type: MemoryType,
        align: usize,
    ) -> Self {
        let start = range.start.align_down(align);
        let end = range.end.align_up(align);
        MemoryDescriptor {
            physical_start: start,
            size_in_bytes: end - start,
            memory_type,
        }
    }

    pub fn new_aligned(
        physical_start: usize,
        size_in_bytes: usize,
        memory_type: MemoryType,
        align: usize,
    ) -> Self {
        let start = physical_start.align_down(align);
        let end = (physical_start + size_in_bytes).align_up(align);
        MemoryDescriptor {
            physical_start: start,
            size_in_bytes: end - start,
            memory_type,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryType {
    #[default]
    Free,
    Ram,
    KImage,
    Reserved,
    Mmio,
    PerCpuData,
}

impl Display for MemoryType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            MemoryType::Free => "Free  ",
            MemoryType::Ram => "RAM   ",
            MemoryType::KImage => "KImg  ",
            MemoryType::Reserved => "Rsv   ",
            MemoryType::Mmio => "MMIO  ",
            MemoryType::PerCpuData => "PerCPU",
        };
        write!(f, "{}", s)
    }
}

impl ranges_ext::RangeOp for MemoryDescriptor {
    type Kind = MemoryType;

    type Type = usize;

    fn range(&self) -> core::ops::Range<Self::Type> {
        self.physical_start..(self.physical_start + self.size_in_bytes)
    }

    fn kind(&self) -> Self::Kind {
        self.memory_type
    }

    fn overwritable(&self, _other: &Self) -> bool {
        matches!(self.memory_type, MemoryType::Free)
    }

    fn clone_with_range(&self, range: core::ops::Range<Self::Type>) -> Self {
        MemoryDescriptor {
            physical_start: range.start,
            size_in_bytes: range.end - range.start,
            memory_type: self.memory_type,
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PageTableInfo {
    pub asid: usize,
    pub addr: usize,
}

impl PageTableInfo {
    pub const fn zero() -> Self {
        PageTableInfo { asid: 0, addr: 0 }
    }
}
