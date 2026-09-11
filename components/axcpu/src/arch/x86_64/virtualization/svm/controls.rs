//! AMD guest/host control pages and interception maps.

use super::{
    super::{Backend, bitmaps, memory::ControlRegion},
    Vmcb, VmcbImage,
};
use crate::{
    PhysAddr,
    virtualization::{ControlMemory, VirtualizationError},
};

/// Host-provided storage for AMD virtualization controls.
pub struct SvmControlMemory<M: ControlMemory> {
    /// One page for guest control and saved state.
    pub guest: M,
    /// One page for the host VMLOAD/VMSAVE bank.
    pub host: M,
    /// Three physically contiguous pages for I/O permissions.
    pub io_permissions: M,
    /// Two physically contiguous pages for MSR permissions.
    pub msr_permissions: M,
}

/// Retains all memory used by one synchronous AMD guest entry.
pub struct SvmControls<M: ControlMemory> {
    guest: Vmcb<M>,
    host: Vmcb<M>,
    io: ControlRegion<M>,
    msr: ControlRegion<M>,
}

impl<M: ControlMemory> SvmControls<M> {
    /// Validates and initializes inactive storage, intercepting all ports/MSRs.
    pub fn new(memory: SvmControlMemory<M>) -> Result<Self, VirtualizationError> {
        let guest = Vmcb::new(memory.guest)?;
        let host = Vmcb::new(memory.host)?;
        let mut io = ControlRegion::new(memory.io_permissions, 3 * 4096, 4096)?;
        let mut msr = ControlRegion::new(memory.msr_permissions, 2 * 4096, 4096)?;
        io.fill(0xff);
        msr.fill(0xff);
        Ok(Self {
            guest,
            host,
            io,
            msr,
        })
    }
    /// Clears a stopped guest image while retaining all control-memory leases.
    /// The caller must configure the image again before entering that guest.
    pub fn reset_guest_image(&mut self) {
        self.guest.reset();
    }

    /// Returns the guest VMCB physical address.
    pub fn guest_address(&self) -> PhysAddr {
        self.guest.physical_address()
    }
    /// Returns the retained host VMLOAD/VMSAVE page address.
    pub fn host_address(&self) -> PhysAddr {
        self.host.physical_address()
    }
    /// Borrows the inactive guest VMCB image.
    pub fn image(&self) -> &VmcbImage {
        self.guest.image()
    }
    /// Exclusively borrows the inactive guest VMCB image.
    pub fn image_mut(&mut self) -> &mut VmcbImage {
        self.guest.image_mut()
    }
    /// Returns the IOPM physical address.
    pub fn io_address(&self) -> PhysAddr {
        self.io.physical_address()
    }
    /// Returns the MSRPM physical address.
    pub fn msr_address(&self) -> PhysAddr {
        self.msr.physical_address()
    }
    /// Sets every representable MSR's read/write interception bit.
    pub fn intercept_all_msrs(&mut self, intercept: bool) {
        self.msr.fill(if intercept { 0xff } else { 0 });
    }
    /// Sets one port's interception bit while guest execution is stopped.
    pub fn set_io_intercept(&mut self, port: u16, intercept: bool) {
        self.io.set_bit(usize::from(port), intercept);
    }
    /// Validates and updates a complete port range without partial failure.
    pub fn set_io_range(
        &mut self,
        first: u16,
        count: u32,
        intercept: bool,
    ) -> Result<(), VirtualizationError> {
        for port in bitmaps::port_range(first, count)? {
            self.set_io_intercept(port as u16, intercept);
        }
        Ok(())
    }
    /// Updates a representable MSR read-interception bit.
    pub fn set_msr_read_intercept(
        &mut self,
        msr: u32,
        intercept: bool,
    ) -> Result<(), VirtualizationError> {
        bitmaps::set_msr(&mut self.msr, Backend::Svm, msr, false, intercept)
    }
    /// Updates a representable MSR write-interception bit.
    pub fn set_msr_write_intercept(
        &mut self,
        msr: u32,
        intercept: bool,
    ) -> Result<(), VirtualizationError> {
        bitmaps::set_msr(&mut self.msr, Backend::Svm, msr, true, intercept)
    }
}
