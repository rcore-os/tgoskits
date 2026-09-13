//! VMX/SVM enable intervals over host-supplied control memory.

use core::marker::PhantomData;

use x86_64::registers::control::{Cr0, Cr4};

use super::memory::ControlRegion;
use crate::virtualization::{ControlMemory, VirtualizationError};

/// Hardware virtualization extension present on the current x86 CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    /// Intel VMX, using VMCS control structures and EPT.
    Vmx,
    /// AMD SVM, using VMCB control structures and nested paging.
    Svm,
}

impl Backend {
    /// Detects the current CPU's implemented virtualization extension.
    pub fn detect() -> Option<Self> {
        use core::arch::x86_64::__cpuid;
        if __cpuid(0).eax >= 1 && __cpuid(1).ecx & (1 << 5) != 0 {
            Some(Self::Vmx)
        } else if __cpuid(0x8000_0000).eax >= 0x8000_0001
            && __cpuid(0x8000_0001).ecx & (1 << 2) != 0
        {
            Some(Self::Svm)
        } else {
            None
        }
    }
}

enum HostState {
    Vmx { cr0: u64, cr4: u64 },
    Svm { efer: u64, hsave: u64 },
}

/// Local VMXON or SVM HSAVE owner, without an allocator or runtime CPU state.
///
/// The host supplies a page-aligned owning memory lease. An active owner cannot
/// move to another thread, and its unsafe enable contract requires CPU pinning.
/// Dropping an active owner retains the lease rather than freeing memory still
/// referenced by hardware. Call [`Self::disable`] before releasing the owner.
pub struct PerCpu<M: ControlMemory> {
    region: Option<ControlRegion<M>>,
    backend: Backend,
    host: Option<HostState>,
    local: PhantomData<*mut ()>,
}

impl<M: ControlMemory> PerCpu<M> {
    /// Validates an inactive, page-aligned control-memory lease of at least 4 KiB.
    pub fn new(memory: M) -> Result<Self, VirtualizationError> {
        let backend = Backend::detect().ok_or(VirtualizationError::Unavailable)?;
        Ok(Self {
            region: Some(ControlRegion::new(memory, 4096, 4096)?),
            backend,
            host: None,
            local: PhantomData,
        })
    }

    /// Returns the selected hardware extension.
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// Reports whether this owner currently holds an enabled hardware interval.
    pub fn is_enabled(&self) -> bool {
        self.host.is_some()
    }

    /// Enables virtualization using this owner's retained control page.
    ///
    /// # Safety
    /// The caller must exclusively own this CPU's virtualization state, remain
    /// on this CPU until disable, and mask IRQs across each transition. VMX must
    /// already be authorized by the boot owner's feature-control configuration.
    pub unsafe fn enable(&mut self) -> Result<(), VirtualizationError> {
        if self.host.is_some() {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        if Backend::detect() != Some(self.backend) {
            return Err(VirtualizationError::Unavailable);
        }
        let region = self
            .region
            .as_mut()
            .expect("inactive owner retains its control lease");
        // SAFETY: the caller owns the privileged local transition. Memory has
        // not been registered with hardware, so initializing it is exclusive.
        let state = unsafe {
            match self.backend {
                Backend::Vmx => enable_vmx(region)?,
                Backend::Svm => enable_svm(region)?,
            }
        };
        self.host = Some(state);
        Ok(())
    }

    /// Disables the hardware interval and restores the original host controls.
    ///
    /// On instruction failure the owner remains active and retains its memory.
    ///
    /// # Safety
    /// The caller must be on the enabling CPU, mask IRQs and ensure that every
    /// guest is stopped, unbound and no longer references this per-CPU state.
    pub unsafe fn disable(&mut self) -> Result<(), VirtualizationError> {
        let host = self.host.as_ref().ok_or(VirtualizationError::NotEnabled)?;
        // SAFETY: all guests have retired and the caller owns the enabling CPU.
        unsafe {
            match *host {
                HostState::Vmx { cr0, cr4 } => {
                    x86::bits64::vmx::vmxoff()
                        .map_err(|_| VirtualizationError::InstructionFailed)?;
                    Cr4::write_raw(cr4);
                    Cr0::write_raw(cr0);
                }
                HostState::Svm { efer, hsave } => {
                    x86::msr::wrmsr(0xc000_0080, efer);
                    x86::msr::wrmsr(0xc001_0117, hsave);
                }
            }
        }
        self.host = None;
        Ok(())
    }

    /// Returns the memory lease after successful retirement.
    /// An active owner is returned unchanged as the error value.
    pub fn into_memory(mut self) -> Result<M, Self> {
        if self.host.is_some() {
            return Err(self);
        }
        Ok(self
            .region
            .take()
            .expect("owner retains its control lease")
            .into_memory())
    }
}

impl<M: ControlMemory> Drop for PerCpu<M> {
    fn drop(&mut self) {
        if self.host.is_some()
            && let Some(region) = self.region.as_mut()
        {
            region.retain_on_failed_retirement();
        }
    }
}

pub(super) fn address_fits<M: ControlMemory>(region: &ControlRegion<M>, bits: u32) -> bool {
    (12..64).contains(&bits) && region.physical_address().as_usize() as u64 <= (1u64 << bits) - 4096
}

pub(super) fn physical_address_bits() -> u32 {
    use core::arch::x86_64::__cpuid;
    if __cpuid(0x8000_0000).eax >= 0x8000_0008 {
        __cpuid(0x8000_0008).eax & 0xff
    } else {
        32
    }
}

unsafe fn enable_vmx<M: ControlMemory>(
    region: &mut ControlRegion<M>,
) -> Result<HostState, VirtualizationError> {
    // SAFETY: the enclosing exclusive transition checked CPUID.VMX.
    unsafe {
        let cr0 = Cr0::read_raw();
        let cr4 = Cr4::read_raw();
        if cr4 & (1 << 13) != 0 {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        let feature_control = x86::msr::rdmsr(0x3a);
        if feature_control & 5 != 5 {
            return Err(VirtualizationError::FeatureControlUnavailable);
        }
        let cr0_fixed0 = x86::msr::rdmsr(0x486);
        let cr0_fixed1 = x86::msr::rdmsr(0x487);
        let cr4_fixed0 = x86::msr::rdmsr(0x488);
        let cr4_fixed1 = x86::msr::rdmsr(0x489);
        let vmx_cr0 = (cr0 | cr0_fixed0) & cr0_fixed1;
        let vmx_cr4 = ((cr4 | cr4_fixed0) & cr4_fixed1) | (1 << 13);
        if vmx_cr0 & cr0_fixed0 != cr0_fixed0
            || vmx_cr4 & cr4_fixed0 != cr4_fixed0
            || vmx_cr4 & !cr4_fixed1 != 0
        {
            return Err(VirtualizationError::Unavailable);
        }
        initialize_vmx_region(region)?;
        Cr0::write_raw(vmx_cr0);
        Cr4::write_raw(vmx_cr4);
        if x86::bits64::vmx::vmxon(region.physical_address().as_usize() as u64).is_err() {
            Cr4::write_raw(cr4);
            Cr0::write_raw(cr0);
            return Err(VirtualizationError::InstructionFailed);
        }
        Ok(HostState::Vmx { cr0, cr4 })
    }
}

unsafe fn enable_svm<M: ControlMemory>(
    region: &mut ControlRegion<M>,
) -> Result<HostState, VirtualizationError> {
    // SAFETY: the enclosing exclusive transition checked CPUID.SVM.
    unsafe {
        if x86::msr::rdmsr(0xc001_0114) & (1 << 4) != 0 {
            return Err(VirtualizationError::Unavailable);
        }
        let efer = x86::msr::rdmsr(0xc000_0080);
        if efer & (1 << 12) != 0 {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        if !address_fits(region, physical_address_bits()) {
            return Err(VirtualizationError::InvalidControlMemory);
        }
        let hsave = x86::msr::rdmsr(0xc001_0117);
        region.clear();
        x86::msr::wrmsr(0xc001_0117, region.physical_address().as_usize() as u64);
        x86::msr::wrmsr(0xc000_0080, efer | (1 << 12));
        Ok(HostState::Svm { efer, hsave })
    }
}

/// Authorizes VMX outside SMX when firmware left IA32_FEATURE_CONTROL unlocked.
///
/// This boot-time lock is irreversible until CPU reset. A locked denial remains
/// an error; ordinary per-CPU enable never changes this firmware policy.
///
/// # Safety
/// The boot owner must have authority to commit this CPU's feature-control policy
/// and exclude other MSR writers. This must precede guest execution.
pub unsafe fn authorize_vmx() -> Result<(), VirtualizationError> {
    if Backend::detect() != Some(Backend::Vmx) {
        return Err(VirtualizationError::Unavailable);
    }
    // SAFETY: the boot owner has authority over the irreversible local policy.
    unsafe {
        let value = x86::msr::rdmsr(0x3a);
        if value & 1 != 0 {
            return if value & 4 != 0 {
                Ok(())
            } else {
                Err(VirtualizationError::FeatureControlUnavailable)
            };
        }
        x86::msr::wrmsr(0x3a, value | 5);
    }
    Ok(())
}

/// Checks the CPU's VMX storage requirements before initializing an inactive lease.
pub(super) unsafe fn initialize_vmx_region<M: ControlMemory>(
    region: &mut ControlRegion<M>,
) -> Result<(), VirtualizationError> {
    // SAFETY: callers check CPUID.VMX and own this inactive control lease at ring 0.
    unsafe {
        let basic = x86::msr::rdmsr(0x480);
        let size = (basic >> 32) & 0x1fff;
        let address_bits = if basic & (1 << 48) != 0 {
            32
        } else {
            physical_address_bits()
        };
        if size == 0
            || size > 4096
            || (basic >> 50) & 0xf != 6
            || !address_fits(region, address_bits)
        {
            return Err(VirtualizationError::InvalidControlMemory);
        }
        region.clear();
        region.write_revision((basic & 0x7fff_ffff) as u32);
    }
    Ok(())
}
