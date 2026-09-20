//! Ownership of the local LVZ enable interval.

use core::marker::PhantomData;

use crate::virtualization::VirtualizationError;

#[derive(Debug)]
struct HostState {
    vector: usize,
    guest_vector: usize,
    status: usize,
}

/// Local LVZ owner, retained until all guest bindings have ended.
#[derive(Debug, Default)]
pub struct PerCpu {
    host: Option<HostState>,
    local: PhantomData<*mut ()>,
}

impl PerCpu {
    /// Creates an inactive owner without touching hardware.
    pub const fn new() -> Self {
        Self {
            host: None,
            local: PhantomData,
        }
    }

    /// Reports whether this owner has enabled a local LVZ interval.
    pub fn is_enabled(&self) -> bool {
        self.host.is_some()
    }

    /// Starts exclusive ownership of this CPU's LVZ state.
    ///
    /// # Safety
    /// The caller must own this CPU's virtualization state, exclude IRQs across
    /// this transition, and retain the object on the same CPU until disable.
    pub unsafe fn enable(&mut self) -> Result<(), VirtualizationError> {
        if self.host.is_some() {
            return Err(VirtualizationError::AlreadyEnabled);
        }
        if !crate::capability::has_hypervisor_extension() {
            return Err(VirtualizationError::Unavailable);
        }
        let vector;
        let guest_vector;
        let status;
        // SAFETY: the capability was checked and the owner excludes CSR races.
        unsafe {
            core::arch::asm!("csrrd {}, 0xc", out(reg) vector, options(nostack));
            core::arch::asm!("gcsrrd {}, 0xc", out(reg) guest_vector, options(nostack));
            core::arch::asm!("csrrd {}, 0x50", out(reg) status, options(nostack));
        }
        self.host = Some(HostState {
            vector,
            guest_vector,
            status,
        });
        Ok(())
    }

    /// Ends the interval and restores the previous host and guest vectors.
    ///
    /// # Safety
    /// All guests must be stopped and unbound. The caller must be on the owning
    /// CPU with IRQs disabled and must retain both saved vector mappings.
    pub unsafe fn disable(&mut self) -> Result<(), VirtualizationError> {
        let host = self.host.take().ok_or(VirtualizationError::NotEnabled)?;
        // SAFETY: all guest references have retired and this owner holds the bank.
        unsafe {
            core::arch::asm!("gcsrwr {}, 0xc", inout(reg) host.guest_vector => _, options(nostack));
            core::arch::asm!("csrwr {}, 0x50", inout(reg) host.status => _, options(nostack));
            core::arch::asm!("csrwr {}, 0xc", inout(reg) host.vector => _, options(nostack));
        }
        Ok(())
    }
}
