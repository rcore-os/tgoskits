//! Real page-table ownership retained across scheduler activation leases.

use core::sync::atomic::{AtomicUsize, Ordering};
use std::{
    os::arceos::{modules::ax_hal, thread},
    sync::Arc,
};

struct Mapping<T> {
    installed: ax_hal::context::InstalledAddressSpace,
    active: Arc<AtomicUsize>,
    _backing: T,
}

struct MappingOwner<T>(Arc<Mapping<T>>);

impl<T> Mapping<T> {
    fn release_cpu(&self, cpu: usize) {
        let bit = 1usize << cpu;
        assert_ne!(self.active.fetch_and(!bit, Ordering::AcqRel) & bit, 0);
    }
}

impl<T: Send + Sync> thread::SchedulerAddressSpaceOwner for Mapping<T> {
    fn release_after_root_switch(self: Arc<Self>, proof: thread::AddressSpaceSwitchProof) {
        self.release_cpu(proof.cpu());
    }

    fn cancel_before_install(self: Arc<Self>, cpu: usize) {
        self.release_cpu(cpu);
    }

    fn abandon(self: Arc<Self>, _cpu: usize) {
        // Keep real tables alive if a runtime loses their retirement proof.
        core::mem::forget(self);
        panic!("user-entry activation abandoned its hardware root");
    }
}

// SAFETY: the test transfers all page tables and backing pages into Mapping.
// The task token retains this Arc until its lazy CPU leases retire. Activation
// publishes the shared target bit before returning, and callbacks clear it
// only after the runtime proves retirement or cancels an uninstalled lease.
// This fixture has one task per mapping; the runtime retains one lease per CPU.
unsafe impl<T: Send + Sync + 'static> thread::UserAddressSpaceOwner for MappingOwner<T> {
    fn prepare_activation(
        &self,
        cpu: usize,
    ) -> Result<thread::SchedulerAddressSpaceActivation, std::os::arceos::task::thread::TaskError>
    {
        let bit = 1usize << cpu;
        assert_eq!(self.0.active.fetch_or(bit, Ordering::AcqRel) & bit, 0);
        Ok(thread::SchedulerAddressSpaceActivation::new(
            self.0.installed,
            cpu,
            self.0.clone(),
        ))
    }

    fn detach_from_task(&self) {
        // The runtime token retains backing storage until its last CPU lease.
    }
}

/// Retains the real tables and pages used by this test's user task.
///
/// # Safety
/// `backing` must own `installed.root()`, all reachable table frames, and every mapped
/// backing page. They must remain valid and at stable addresses until dropped.
pub(super) unsafe fn new<T: Send + Sync + 'static>(
    installed: ax_hal::context::InstalledAddressSpace,
    backing: T,
) -> (thread::TaskAddressSpace, Arc<thread::AddressSpaceCpuState>) {
    assert!(installed.is_user());
    assert!(
        installed.hardware_tag() == 0
            || u32::from(installed.hardware_tag()) < ax_cpu::mmu::address_space_tag_capacity()
    );
    let root = installed.root();
    let active = Arc::new(AtomicUsize::new(0));
    let cpu_state = Arc::new(thread::AddressSpaceCpuState::with_mm_active_mask(
        root,
        active.clone(),
    ));
    let owner = Arc::new(Mapping {
        installed,
        active,
        _backing: backing,
    });
    let task = thread::TaskAddressSpace::new_managed(root, cpu_state.clone(), MappingOwner(owner))
        .unwrap();
    (task, cpu_state)
}
