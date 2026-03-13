#![cfg_attr(not(test), no_std)]
#![cfg_attr(doc, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

use core::fmt;

use memory_addr::PhysAddr;

mod arch;
pub use self::arch::*;

bitflags::bitflags! {
    /// Generic page table entry flags that indicate the corresponding mapped
    /// memory region permissions and attributes.
    #[derive(Clone, Copy, PartialEq)]
    pub struct MappingFlags: usize {
        /// The memory is readable.
        const READ          = 1 << 0;
        /// The memory is writable.
        const WRITE         = 1 << 1;
        /// The memory is executable.
        const EXECUTE       = 1 << 2;
        /// The memory is user accessible.
        const USER          = 1 << 3;
        /// The memory is device memory.
        const DEVICE        = 1 << 4;
        /// The memory is uncached.
        const UNCACHED      = 1 << 5;
    }
}

impl fmt::Debug for MappingFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// A generic page table entry.
///
/// All architecture-specific page table entry types implement this trait.
pub trait GenericPTE: fmt::Debug + Clone + Copy + Sync + Send + Sized {
    /// Creates a page table entry point to a terminate page or block.
    fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self;
    /// Creates a page table entry point to a next level page table.
    fn new_table(paddr: PhysAddr) -> Self;

    /// Returns the physical address mapped by this entry.
    fn paddr(&self) -> PhysAddr;
    /// Returns the flags of this entry.
    fn flags(&self) -> MappingFlags;

    /// Set mapped physical address of the entry.
    fn set_paddr(&mut self, paddr: PhysAddr);
    /// Set flags of the entry.
    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool);

    /// Returns the raw bits of this entry.
    fn bits(self) -> usize;
    /// Returns whether this entry is zero.
    fn is_unused(&self) -> bool;
    /// Returns whether this entry flag indicates present.
    fn is_present(&self) -> bool;
    /// For non-last level translation, returns whether this entry maps to a
    /// huge frame.
    fn is_huge(&self) -> bool;
    /// Set this entry to zero.
    fn clear(&mut self);
}
