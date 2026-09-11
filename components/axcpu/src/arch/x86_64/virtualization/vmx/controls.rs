//! VMCS and its interception-map leases have one retirement owner.

use super::{
    super::{Backend, bitmaps, memory::ControlRegion},
    Vmcs,
    fields::{VmcsField, VmcsReadWrite, VmcsValue},
};
use crate::{
    PhysAddr,
    virtualization::{ControlMemory, VirtualizationError},
};

/// Host-provided storage for Intel virtualization controls.
/// Pages need not be physically adjacent; each lease remains independently owned.
pub struct VmxControlMemory<M: ControlMemory> {
    /// One page for the VMCS.
    pub vmcs: M,
    /// One page for I/O ports 0 through 32767.
    pub io_bitmap_a: M,
    /// One page for I/O ports 32768 through 65535.
    pub io_bitmap_b: M,
    /// One page for the VMX MSR bitmap.
    pub msr_bitmap: M,
}

/// Intel VMCS and the hardware permission maps referenced by it.
///
/// If an active binding is dropped, every control lease is retained. This
/// prevents the VMCS from retaining references to already freed bitmap pages.
pub struct VmxControls<M: ControlMemory> {
    vmcs: Vmcs<M>,
    io: [ControlRegion<M>; 2],
    msr: ControlRegion<M>,
}

impl<M: ControlMemory> VmxControls<M> {
    /// Validates host leases and initializes inactive hardware storage.
    /// Interception defaults to all ports and MSRs; the host chooses any bypasses.
    ///
    /// # Safety
    /// The caller must satisfy [`Vmcs::new`]'s VMX and ring-0 requirements.
    /// All leases must be inactive and disjoint, as required by `ControlMemory`.
    pub unsafe fn new(memory: VmxControlMemory<M>) -> Result<Self, VirtualizationError> {
        let mut io = [
            ControlRegion::new(memory.io_bitmap_a, 4096, 4096)?,
            ControlRegion::new(memory.io_bitmap_b, 4096, 4096)?,
        ];
        let mut msr = ControlRegion::new(memory.msr_bitmap, 4096, 4096)?;
        for region in &mut io {
            region.fill(0xff);
        }
        msr.fill(0xff);
        // SAFETY: the caller owns an inactive VMCS lease on a VMX-capable CPU.
        let vmcs = unsafe { Vmcs::new(memory.vmcs)? };
        Ok(Self { vmcs, io, msr })
    }

    /// Borrows the checked current-VMCS interface.
    pub fn vmcs(&self) -> &Vmcs<M> {
        &self.vmcs
    }
    /// Binds this control set on the current CPU.
    ///
    /// # Safety
    /// The caller must satisfy [`Vmcs::bind`]'s CPU pin, IRQ and ownership contract.
    pub unsafe fn bind(&mut self) -> Result<(), VirtualizationError> {
        // SAFETY: this control set retains all owned hardware references and
        // the caller owns the corresponding current-CPU binding interval.
        unsafe { self.vmcs.bind() }
    }

    /// Retires this control set's VMCS binding before its memory can be released.
    ///
    /// # Safety
    /// The caller must satisfy [`Vmcs::unbind`]'s quiescent-guest CPU contract.
    pub unsafe fn unbind(&mut self) -> Result<(), VirtualizationError> {
        // SAFETY: the caller owns the binding CPU and has stopped guest entry.
        unsafe { self.vmcs.unbind() }
    }

    /// Reads a typed field while the control set is bound.
    pub fn read<T: VmcsValue, A>(&self, field: VmcsField<T, A>) -> Result<T, VirtualizationError> {
        self.vmcs.read(field)
    }

    /// Writes a typed field while the control set is bound.
    pub fn write<T: VmcsValue>(
        &mut self,
        field: VmcsField<T, VmcsReadWrite>,
        value: T,
    ) -> Result<(), VirtualizationError> {
        self.vmcs.write(field, value)
    }

    /// Returns the two hardware I/O bitmap addresses in A/B order.
    pub fn io_addresses(&self) -> [PhysAddr; 2] {
        [self.io[0].physical_address(), self.io[1].physical_address()]
    }
    /// Returns the hardware MSR bitmap address.
    pub fn msr_address(&self) -> PhysAddr {
        self.msr.physical_address()
    }
    /// Sets every representable MSR's read/write interception bit.
    pub fn intercept_all_msrs(&mut self, intercept: bool) {
        self.msr.fill(if intercept { 0xff } else { 0 });
    }
    /// Sets one port's interception bit while guest execution is stopped.
    pub fn set_io_intercept(&mut self, port: u16, intercept: bool) {
        let port = usize::from(port);
        self.io[port / 32768].set_bit(port % 32768, intercept);
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
        bitmaps::set_msr(&mut self.msr, Backend::Vmx, msr, false, intercept)
    }
    /// Updates a representable MSR write-interception bit.
    pub fn set_msr_write_intercept(
        &mut self,
        msr: u32,
        intercept: bool,
    ) -> Result<(), VirtualizationError> {
        bitmaps::set_msr(&mut self.msr, Backend::Vmx, msr, true, intercept)
    }
}

impl<M: ControlMemory> Drop for VmxControls<M> {
    fn drop(&mut self) {
        if self.vmcs.is_bound() {
            for region in &mut self.io {
                region.retain_on_failed_retirement();
            }
            self.msr.retain_on_failed_retirement();
        }
    }
}
