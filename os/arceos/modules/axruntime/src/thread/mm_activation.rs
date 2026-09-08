//! Allocation-free ownership transferred at the runtime's root-switch boundary.

use alloc::sync::Arc;

use ax_hal::context::InstalledAddressSpace;

use super::TaskError;

/// Evidence that this CPU has replaced its previous hardware root.
#[derive(Debug)]
pub struct AddressSpaceSwitchProof {
    cpu: usize,
}

impl AddressSpaceSwitchProof {
    #[cfg(feature = "uspace")]
    pub(super) const fn new(cpu: usize) -> Self {
        Self { cpu }
    }

    /// Returns the CPU whose old translations are no longer installed.
    pub const fn cpu(&self) -> usize {
        self.cpu
    }
}

/// Preallocated MM accounting retained by one CPU activation.
///
/// These callbacks run with IRQs disabled. They must not allocate, sleep, or
/// destroy the last lifetime anchor for the page tables or their backing store.
pub trait SchedulerAddressSpaceOwner: Send + Sync {
    /// Releases accounting after the runtime completed a hardware root switch.
    fn release_after_root_switch(self: Arc<Self>, proof: AddressSpaceSwitchProof);
    /// Cancels a reservation which the runtime never installed in hardware.
    fn cancel_before_install(self: Arc<Self>, cpu: usize);
    /// Retains an installed root for which no retirement proof was obtained.
    fn abandon(self: Arc<Self>, cpu: usize);
}

/// OS ownership held by a runtime task token until its last CPU lease drains.
///
/// # Safety
/// Every prepared activation must own the declared root and publish its CPU in
/// the MM's TLB target set before returning. The root must remain valid until
/// the activation is released. Detaching a task must retain storage needed by
/// lazy CPUs; activation callbacks must not perform a final storage release.
pub unsafe trait UserAddressSpaceOwner: Send + Sync {
    /// Acquires a CPU activation without allocation, blocking, or hardware I/O.
    fn prepare_activation(&self, cpu: usize) -> Result<SchedulerAddressSpaceActivation, TaskError>;
    /// Drops task-scoped ownership in ordinary task context, exactly once.
    fn detach_from_task(&self);
}

/// An inline reservation, subsequently owned by the CPU that installs it.
///
/// The owner Arc already exists; unsizing or cloning it does not allocate.
pub struct SchedulerAddressSpaceActivation {
    installed: InstalledAddressSpace,
    cpu: usize,
    owner: Option<Arc<dyn SchedulerAddressSpaceOwner>>,
    committed: bool,
}

impl SchedulerAddressSpaceActivation {
    /// Transfers an acquired, not yet installed MM activation to the runtime.
    pub fn new(
        installed: InstalledAddressSpace,
        cpu: usize,
        owner: Arc<dyn SchedulerAddressSpaceOwner>,
    ) -> Self {
        Self {
            installed,
            cpu,
            owner: Some(owner),
            committed: false,
        }
    }

    /// Returns the complete root, hardware tag, generation and epoch identity.
    pub const fn installed(&self) -> InstalledAddressSpace {
        self.installed
    }

    #[cfg(feature = "uspace")]
    pub(super) fn commit(&mut self, cpu: usize) {
        assert_eq!(self.cpu, cpu);
        assert!(!self.committed);
        self.committed = true;
    }

    #[cfg(feature = "uspace")]
    pub(super) fn release(mut self, proof: AddressSpaceSwitchProof) {
        assert_eq!(self.cpu, proof.cpu());
        assert!(self.committed);
        self.owner
            .take()
            .expect("activation is consumed once")
            .release_after_root_switch(proof);
    }
}

impl Drop for SchedulerAddressSpaceActivation {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            if self.committed {
                owner.abandon(self.cpu);
            } else {
                owner.cancel_before_install(self.cpu);
            }
        }
    }
}
