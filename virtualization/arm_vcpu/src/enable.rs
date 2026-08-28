//! Ordering contract for publishing AArch64 EL2 virtualization state.

use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{ArmHostPageFaultAccess, ArmVcpuError, ArmVcpuResult};

pub(crate) type HostIrqCallback = fn();
pub(crate) type HostSyncCallback = fn(&mut usize, usize, ArmHostPageFaultAccess, bool) -> bool;

pub(crate) trait El2EnableOperations {
    type HostHooks;

    fn install_host_hooks(&mut self) -> ArmVcpuResult<Self::HostHooks>;
    fn install_exception_vector(&mut self);
    fn synchronize_context(&mut self);
    fn enable_virtualization(&mut self);
}

pub(crate) fn enable_el2<O: El2EnableOperations>(
    operations: &mut O,
) -> ArmVcpuResult<O::HostHooks> {
    let hooks = operations.install_host_hooks()?;
    operations.install_exception_vector();
    operations.synchronize_context();
    operations.enable_virtualization();
    operations.synchronize_context();
    Ok(hooks)
}

pub(crate) trait El2DisableOperations {
    fn validate_host_hooks(&mut self) -> ArmVcpuResult;
    fn restore_exception_vector(&mut self);
    fn synchronize_context(&mut self);
    fn release_host_hooks(&mut self);
    fn disable_virtualization(&mut self);
}

pub(crate) fn disable_el2<O: El2DisableOperations>(operations: &mut O) -> ArmVcpuResult {
    operations.validate_host_hooks()?;
    operations.restore_exception_vector();
    operations.synchronize_context();
    operations.release_host_hooks();
    operations.disable_virtualization();
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct HostHookSet {
    irq: HostIrqCallback,
    synchronous_fault: HostSyncCallback,
}

impl HostHookSet {
    pub(crate) const fn new(irq: HostIrqCallback, synchronous_fault: HostSyncCallback) -> Self {
        Self {
            irq,
            synchronous_fault,
        }
    }

    fn irq_address(self) -> usize {
        self.irq as *const () as usize
    }

    fn synchronous_fault_address(self) -> usize {
        self.synchronous_fault as *const () as usize
    }
}

pub(crate) struct HostHookRegistry {
    locked: AtomicBool,
    irq: AtomicUsize,
    synchronous_fault: AtomicUsize,
    users: AtomicUsize,
}

impl HostHookRegistry {
    pub(crate) const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            irq: AtomicUsize::new(0),
            synchronous_fault: AtomicUsize::new(0),
            users: AtomicUsize::new(0),
        }
    }

    pub(crate) fn install(&self, hooks: HostHookSet) -> ArmVcpuResult {
        let _guard = self.lock();
        let users = self.users.load(Ordering::Relaxed);
        if users == 0 {
            self.irq.store(hooks.irq_address(), Ordering::Release);
            self.synchronous_fault
                .store(hooks.synchronous_fault_address(), Ordering::Release);
            self.users.store(1, Ordering::Release);
            return Ok(());
        }
        if !self.matches_locked(hooks) {
            return Err(ArmVcpuError::BadState);
        }

        let users = users.checked_add(1).ok_or(ArmVcpuError::BadState)?;
        self.users.store(users, Ordering::Release);
        Ok(())
    }

    pub(crate) fn validate_release(&self, hooks: HostHookSet) -> ArmVcpuResult {
        let _guard = self.lock();
        if self.users.load(Ordering::Relaxed) == 0 || !self.matches_locked(hooks) {
            return Err(ArmVcpuError::BadState);
        }
        Ok(())
    }

    pub(crate) fn release_validated(&self, hooks: HostHookSet) {
        let _guard = self.lock();
        let users = self.users.load(Ordering::Relaxed);
        assert!(
            users != 0 && self.matches_locked(hooks),
            "validated arm_vcpu host hooks changed before release"
        );
        if users > 1 {
            self.users.store(users - 1, Ordering::Release);
            return;
        }

        self.users.store(0, Ordering::Release);
        self.irq.store(0, Ordering::Release);
        self.synchronous_fault.store(0, Ordering::Release);
    }

    pub(crate) fn irq(&self) -> Option<HostIrqCallback> {
        let address = self.irq.load(Ordering::Acquire);
        if address == 0 {
            return None;
        }
        // SAFETY: only typed `HostIrqCallback` values are erased into this atomic.
        Some(unsafe { core::mem::transmute::<usize, HostIrqCallback>(address) })
    }

    pub(crate) fn synchronous_fault(&self) -> Option<HostSyncCallback> {
        let address = self.synchronous_fault.load(Ordering::Acquire);
        if address == 0 {
            return None;
        }
        // SAFETY: only typed `HostSyncCallback` values are erased into this atomic.
        Some(unsafe { core::mem::transmute::<usize, HostSyncCallback>(address) })
    }

    #[cfg(test)]
    fn user_count(&self) -> usize {
        self.users.load(Ordering::Acquire)
    }

    fn matches_locked(&self, hooks: HostHookSet) -> bool {
        self.irq.load(Ordering::Relaxed) == hooks.irq_address()
            && self.synchronous_fault.load(Ordering::Relaxed) == hooks.synchronous_fault_address()
    }

    fn lock(&self) -> HostHookRegistryGuard<'_> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        HostHookRegistryGuard { registry: self }
    }
}

struct HostHookRegistryGuard<'a> {
    registry: &'a HostHookRegistry,
}

impl Drop for HostHookRegistryGuard<'_> {
    fn drop(&mut self) {
        self.registry.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::ArmHostPageFaultAccess;

    fn host_a_irq() {}

    fn host_a_sync(
        _saved_pc: &mut usize,
        _fault_addr: usize,
        _access: ArmHostPageFaultAccess,
        _parent_irqs_enabled: bool,
    ) -> bool {
        false
    }

    fn host_b_irq() {}

    fn host_b_sync(
        _saved_pc: &mut usize,
        _fault_addr: usize,
        _access: ArmHostPageFaultAccess,
        _parent_irqs_enabled: bool,
    ) -> bool {
        false
    }

    fn host_a_hooks() -> HostHookSet {
        HostHookSet::new(host_a_irq, host_a_sync)
    }

    fn host_b_hooks() -> HostHookSet {
        HostHookSet::new(host_b_irq, host_b_sync)
    }

    #[derive(Default)]
    struct RecordingEl2Operations {
        events: std::vec::Vec<&'static str>,
        fail_install: bool,
        fail_validation: bool,
    }

    impl El2EnableOperations for RecordingEl2Operations {
        type HostHooks = usize;

        fn install_host_hooks(&mut self) -> ArmVcpuResult<Self::HostHooks> {
            self.events.push("install-hooks");
            if self.fail_install {
                Err(ArmVcpuError::BadState)
            } else {
                Ok(7)
            }
        }

        fn install_exception_vector(&mut self) {
            self.events.push("vbar");
        }

        fn synchronize_context(&mut self) {
            self.events.push("isb");
        }

        fn enable_virtualization(&mut self) {
            self.events.push("hcr");
        }
    }

    impl El2DisableOperations for RecordingEl2Operations {
        fn validate_host_hooks(&mut self) -> ArmVcpuResult {
            self.events.push("validate-hooks");
            if self.fail_validation {
                Err(ArmVcpuError::BadState)
            } else {
                Ok(())
            }
        }

        fn restore_exception_vector(&mut self) {
            self.events.push("restore-vbar");
        }

        fn synchronize_context(&mut self) {
            self.events.push("isb");
        }

        fn release_host_hooks(&mut self) {
            self.events.push("release-hooks");
        }

        fn disable_virtualization(&mut self) {
            self.events.push("disable-hcr");
        }
    }

    #[test]
    fn enable_publishes_paired_hooks_then_vbar_isb_then_hcr_isb() {
        let mut operations = RecordingEl2Operations::default();

        assert_eq!(enable_el2(&mut operations).unwrap(), 7);
        assert_eq!(
            operations.events,
            ["install-hooks", "vbar", "isb", "hcr", "isb"]
        );
    }

    #[test]
    fn enable_stops_before_vector_publication_when_hook_install_fails() {
        let mut operations = RecordingEl2Operations {
            fail_install: true,
            ..Default::default()
        };

        assert_eq!(enable_el2(&mut operations), Err(ArmVcpuError::BadState));
        assert_eq!(operations.events, ["install-hooks"]);
    }

    #[test]
    fn disable_restores_vbar_then_isb_then_releases_paired_hooks() {
        let mut operations = RecordingEl2Operations::default();

        disable_el2(&mut operations).unwrap();
        assert_eq!(
            operations.events,
            [
                "validate-hooks",
                "restore-vbar",
                "isb",
                "release-hooks",
                "disable-hcr",
            ]
        );
    }

    #[test]
    fn disable_validation_failure_preserves_all_published_state() {
        let mut operations = RecordingEl2Operations {
            fail_validation: true,
            ..Default::default()
        };

        assert_eq!(disable_el2(&mut operations), Err(ArmVcpuError::BadState));
        assert_eq!(operations.events, ["validate-hooks"]);
    }

    #[test]
    fn paired_hooks_share_one_multi_cpu_refcount_for_the_same_host() {
        let registry = HostHookRegistry::new();
        let hooks = host_a_hooks();

        registry.install(hooks).unwrap();
        registry.install(hooks).unwrap();

        assert!(core::ptr::fn_addr_eq(
            registry.irq().unwrap(),
            host_a_irq as HostIrqCallback
        ));
        assert!(core::ptr::fn_addr_eq(
            registry.synchronous_fault().unwrap(),
            host_a_sync as HostSyncCallback
        ));
        assert_eq!(registry.user_count(), 2);
        registry.validate_release(hooks).unwrap();
        registry.release_validated(hooks);
        assert_eq!(registry.user_count(), 1);
        registry.validate_release(hooks).unwrap();
        registry.release_validated(hooks);
        assert_eq!(registry.user_count(), 0);
        assert!(registry.irq().is_none());
        assert!(registry.synchronous_fault().is_none());
    }

    #[test]
    fn conflicting_host_install_rolls_back_without_incrementing_users() {
        let registry = HostHookRegistry::new();
        let installed = host_a_hooks();
        let conflicting = host_b_hooks();
        registry.install(installed).unwrap();

        assert_eq!(
            registry.install(conflicting),
            Err(crate::ArmVcpuError::BadState)
        );
        assert_eq!(registry.user_count(), 1);
    }

    #[test]
    fn invalid_or_duplicate_release_is_rejected_before_state_changes() {
        let registry = HostHookRegistry::new();
        let installed = host_a_hooks();
        let wrong_host = host_b_hooks();
        registry.install(installed).unwrap();

        assert_eq!(
            registry.validate_release(wrong_host),
            Err(crate::ArmVcpuError::BadState)
        );
        assert_eq!(registry.user_count(), 1);

        registry.validate_release(installed).unwrap();
        registry.release_validated(installed);
        assert_eq!(
            registry.validate_release(installed),
            Err(crate::ArmVcpuError::BadState)
        );
        assert_eq!(registry.user_count(), 0);
    }
}
