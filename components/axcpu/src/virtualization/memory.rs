//! Guest address coordinates consumed by CPU translation instructions.

ax_memory_addr::def_usize_addr! {
    /// Guest physical address interpreted by the active second-stage translation.
    pub type GuestPhysAddr;
    /// Guest virtual address interpreted by the active guest translation regime.
    pub type GuestVirtAddr;
}
ax_memory_addr::def_usize_addr_formatter! {
    GuestPhysAddr = "GPA:{}";
    GuestVirtAddr = "GVA:{}";
}

/// An owning lease over stable initialized CPU state storage.
///
/// Allocation and mapping policy belong to the host. CPU code consumes only
/// the writable virtual range; physical contiguity is not required.
///
/// # Safety
/// The pointer and length must describe the same exclusive, initialized,
/// writable range for the lease's entire lifetime, including all backing and
/// mappings. Forgetting the lease must retain that allocation and mapping.
/// No other owner may access or reuse the range while leased. Both methods
/// must be stable, nonblocking and allocation-free. The range must satisfy
/// Rust pointer range requirements, including a length no greater than
/// `isize::MAX` and no address wraparound.
///
/// If the implementor also implements `Clone`, cloning must preserve contents,
/// length, minimum guaranteed alignment and every initialization/lifetime
/// guarantee above, with independent backing that aliases neither the source
/// nor any other live lease. In particular, if the source virtual address is
/// aligned for `usize`, the cloned virtual address must also be aligned for
/// `usize`, even when the type promises a weaker minimum alignment. CPU state
/// constructors validate this alignment once and rely on it surviving Clone.
pub unsafe trait StateMemory {
    /// Writable virtual start of the owned range.
    fn virtual_address(&self) -> core::ptr::NonNull<u8>;
    /// Size of the owned range in bytes.
    fn byte_len(&self) -> usize;
}

/// An owning lease over physically contiguous hardware control storage.
///
/// # Safety
/// In addition to [`StateMemory`], the physical address must identify the same
/// range, backed by coherent write-back memory suitable for CPU control
/// structures. The physical address is stable, nonblocking and allocation-free.
/// Hardware must retire all references before the lease may be reclaimed;
/// forgetting it retains both allocation and mapping if retirement fails.
#[cfg(target_arch = "x86_64")]
pub unsafe trait ControlMemory: StateMemory {
    /// Physical start of the contiguous range.
    fn physical_address(&self) -> crate::PhysAddr;
}
