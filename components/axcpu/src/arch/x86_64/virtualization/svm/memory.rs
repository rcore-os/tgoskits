//! Host-owned VMCB storage with checked hardware layout.

use core::marker::PhantomData;

use super::{
    super::{
        memory::ControlRegion,
        percpu::{address_fits, physical_address_bits},
    },
    VmcbImage,
};
use crate::{
    PhysAddr,
    virtualization::{ControlMemory, VirtualizationError},
};

/// A zero-initialized VMCB image backed by a host-owned memory lease.
///
/// Unlike a VMCS, a VMCB is not made current by loading a pointer into the CPU.
/// Hardware accesses it during VMRUN, VMLOAD or VMSAVE. Those operations must
/// exclusively borrow the image and finish before the lease can be released.
/// The image cannot be shared between CPUs through this type.
pub struct Vmcb<M: ControlMemory> {
    region: ControlRegion<M>,
    local: PhantomData<*mut ()>,
}

impl<M: ControlMemory> Vmcb<M> {
    /// Validates a page-aligned lease against MAXPHYADDR and clears the image.
    /// This only constructs storage; it does not enable SVM or enter a guest.
    pub fn new(memory: M) -> Result<Self, VirtualizationError> {
        let mut region = ControlRegion::new(memory, size_of::<VmcbImage>(), 4096)?;
        if !address_fits(&region, physical_address_bits()) {
            return Err(VirtualizationError::InvalidControlMemory);
        }
        region.clear();
        Ok(Self {
            region,
            local: PhantomData,
        })
    }

    /// Returns the physical operand used by SVM instructions.
    /// Any instruction using this address must finish before releasing the owner.
    pub fn physical_address(&self) -> PhysAddr {
        self.region.physical_address()
    }

    /// Borrows the inactive image for inspection or configuration.
    pub fn image(&self) -> &VmcbImage {
        // SAFETY: construction validates a complete aligned image and initializes
        // all bytes. Its fields accept all integer bit patterns. The exclusive
        // lease retains the mapping; no hardware operation can outlive its borrow.
        unsafe { self.region.pointer().cast::<VmcbImage>().as_ref() }
    }

    /// Exclusively borrows the inactive image for configuration.
    pub fn image_mut(&mut self) -> &mut VmcbImage {
        // SAFETY: the region owns the initialized image and the exclusive borrow
        // excludes other safe image access for the returned reference's lifetime.
        unsafe { self.region.pointer().cast::<VmcbImage>().as_mut() }
    }

    /// Saves the current CPU's VMLOAD/VMSAVE register bank into this image.
    ///
    /// # Safety
    /// The caller must execute at ring 0 with SVM enabled on the current CPU.
    /// CPU migration and conflicting control-state transitions must be excluded
    /// until the instruction completes. This image must not be used by hardware
    /// through an independently published physical address during the call.
    pub unsafe fn save_current_state(&mut self) {
        // SAFETY: the caller owns the enabled SVM CPU; this exclusive borrow
        // retains the validated VMCB mapping throughout the synchronous write.
        unsafe {
            core::arch::asm!(
                "vmsave rax",
                in("rax") self.physical_address().as_usize(),
                options(nostack, preserves_flags),
            );
        }
    }

    pub(super) fn reset(&mut self) {
        self.region.clear();
    }

    /// Returns the host's allocation lease once image borrowing has ended.
    pub fn into_memory(self) -> M {
        self.region.into_memory()
    }
}
