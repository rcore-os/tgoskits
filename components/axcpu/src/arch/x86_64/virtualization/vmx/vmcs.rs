//! VMCS memory ownership and typed field access.

use core::marker::PhantomData;

use super::{
    super::{memory::ControlRegion, percpu::initialize_vmx_region},
    fields::{self, VmcsField, VmcsReadWrite, VmcsValue},
};
use crate::{
    PhysAddr,
    virtualization::{Backend, ControlMemory, VirtualizationError},
};

/// An owned VMCS control region with an explicit current-CPU binding interval.
///
/// Field access is available only while bound. Its unsafe binding contract
/// excludes other VMCS owners and CPU migration until unbind. Dropping a bound
/// VMCS retains its memory lease so hardware cannot reference freed storage.
pub struct Vmcs<M: ControlMemory> {
    region: Option<ControlRegion<M>>,
    initialized: bool,
    bound: bool,
    local: PhantomData<*mut ()>,
}

impl<M: ControlMemory> Vmcs<M> {
    /// Initializes the VMCS revision header in a host-provided control page.
    ///
    /// # Safety
    /// The caller must execute at ring 0 on a CPU with VMX available. The memory
    /// lease must not have been installed in any hardware control structure.
    pub unsafe fn new(memory: M) -> Result<Self, VirtualizationError> {
        if Backend::detect() != Some(Backend::Vmx) {
            return Err(VirtualizationError::Unavailable);
        }
        let mut region = ControlRegion::new(memory, 4096, 4096)?;
        // SAFETY: VMX was detected and the caller provides an inactive lease.
        unsafe { initialize_vmx_region(&mut region)? };
        Ok(Self {
            region: Some(region),
            initialized: false,
            bound: false,
            local: PhantomData,
        })
    }

    /// Returns the retained control region's physical address.
    pub fn physical_address(&self) -> PhysAddr {
        self.region
            .as_ref()
            .expect("VMCS retains its lease until retirement")
            .physical_address()
    }

    /// Reports whether this owner still holds a current-CPU hardware binding.
    pub const fn is_bound(&self) -> bool {
        self.bound
    }

    /// Selects this VMCS on the current CPU.
    ///
    /// # Safety
    /// VMX must be enabled on this CPU. The caller must exclude other current
    /// VMCS owners and retain the CPU pin and control storage until unbind.
    /// Each transition must execute with IRQs disabled.
    pub unsafe fn bind(&mut self) -> Result<(), VirtualizationError> {
        if self.bound {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        let address = self.physical_address().as_usize() as u64;
        // SAFETY: the caller owns this enabled VMX CPU and the retained page.
        unsafe {
            if !self.initialized {
                x86::bits64::vmx::vmclear(address)
                    .map_err(|_| VirtualizationError::InstructionFailed)?;
                self.initialized = true;
            }
            x86::bits64::vmx::vmptrld(address)
                .map_err(|_| VirtualizationError::InstructionFailed)?;
        }
        self.bound = true;
        Ok(())
    }

    /// Clears the VMCS hardware binding and launch state.
    ///
    /// An instruction failure preserves the bound state and memory lease.
    ///
    /// # Safety
    /// The caller must own the binding CPU, mask IRQs and ensure the guest has
    /// stopped before releasing its current VMCS.
    pub unsafe fn unbind(&mut self) -> Result<(), VirtualizationError> {
        if !self.bound {
            return Err(VirtualizationError::NotEnabled);
        }
        // SAFETY: guest execution ended and the caller owns the binding CPU.
        unsafe { x86::bits64::vmx::vmclear(self.physical_address().as_usize() as u64) }
            .map_err(|_| VirtualizationError::InstructionFailed)?;
        self.bound = false;
        Ok(())
    }

    /// Reads a field with its architectural value width from this binding.
    pub fn read<T: VmcsValue, A>(&self, field: VmcsField<T, A>) -> Result<T, VirtualizationError> {
        if !self.bound {
            return Err(VirtualizationError::NotEnabled);
        }
        // SAFETY: the unsafe bind contract retains exclusive current-VMCS
        // ownership and the CPU pin; sealed fields carry valid encodings.
        unsafe { x86::bits64::vmx::vmread(field.encoding()) }
            .map(fields::from_raw)
            .map_err(|_| VirtualizationError::InstructionFailed)
    }

    /// Writes a writable field; read-only fields cannot be passed here.
    /// Hardware validates guest and host state when VM entry is requested.
    pub fn write<T: VmcsValue>(
        &mut self,
        field: VmcsField<T, VmcsReadWrite>,
        value: T,
    ) -> Result<(), VirtualizationError> {
        if !self.bound {
            return Err(VirtualizationError::NotEnabled);
        }
        // SAFETY: this VMCS owns the current hardware binding, and the field's
        // access marker excludes writes to read-only fields.
        unsafe { x86::bits64::vmx::vmwrite(field.encoding(), fields::into_raw(value)) }
            .map_err(|_| VirtualizationError::InstructionFailed)
    }

    /// Returns the control lease if no CPU can still reference this VMCS.
    /// A bound owner is returned unchanged in the error value.
    pub fn into_memory(mut self) -> Result<M, Self> {
        if self.bound {
            return Err(self);
        }
        Ok(self
            .region
            .take()
            .expect("VMCS retains its lease until retirement")
            .into_memory())
    }
}

impl<M: ControlMemory> Drop for Vmcs<M> {
    fn drop(&mut self) {
        if self.bound
            && let Some(region) = self.region.as_mut()
        {
            region.retain_on_failed_retirement();
        }
    }
}
