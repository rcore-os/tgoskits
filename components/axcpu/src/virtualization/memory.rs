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

/// An owning lease over stable, physically contiguous hardware control storage.
///
/// The host allocates the storage and supplies this lease. CPU code knows only
/// its addresses and size; allocation, direct-map policy and accounting remain
/// with the host. A successfully retired control object returns its lease.
///
/// # Safety
/// The addresses must identify the same exclusive, initialized, writable range
/// for the lease's entire lifetime. The mapping must be coherent write-back
/// memory suitable for CPU control structures. Address and size methods must
/// return stable values, and no other owner may access or reuse the range while
/// leased. Forgetting the lease must keep the backing allocation and mapping
/// alive: a borrowed slice whose owner can subsequently free it is insufficient.
/// This permits retaining memory if hardware retirement fails.
#[cfg(target_arch = "x86_64")]
pub unsafe trait ControlMemory {
    /// Physical start of the contiguous range.
    fn physical_address(&self) -> crate::PhysAddr;
    /// Writable virtual alias of the same range.
    fn virtual_address(&self) -> core::ptr::NonNull<u8>;
    /// Size of the range in bytes.
    fn byte_len(&self) -> usize;
}
