//! IRQ-safe ownership state for one CPU's GIC maintenance observation.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use axvm_types::{VCpuId, VMId, VmBackendError, VmBackendResult};

const NO_OWNER: usize = usize::MAX;

pub(super) struct MaintenancePublication {
    vm_id: AtomicUsize,
    vcpu_id: AtomicUsize,
    generation: AtomicU64,
    seen: AtomicBool,
}

impl MaintenancePublication {
    pub(super) const fn new() -> Self {
        Self {
            vm_id: AtomicUsize::new(NO_OWNER),
            vcpu_id: AtomicUsize::new(NO_OWNER),
            generation: AtomicU64::new(0),
            seen: AtomicBool::new(false),
        }
    }

    pub(super) fn publish(&self, vm_id: VMId, vcpu_id: VCpuId, generation: u64) -> VmBackendResult {
        if generation == 0 || self.generation.load(Ordering::Relaxed) != 0 {
            return Err(VmBackendError::InvalidState);
        }
        self.vm_id.store(vm_id, Ordering::Relaxed);
        self.vcpu_id.store(vcpu_id, Ordering::Relaxed);
        self.seen.store(false, Ordering::Relaxed);
        self.generation.store(generation, Ordering::Release);
        Ok(())
    }

    pub(super) fn consume(
        &self,
        vm_id: VMId,
        vcpu_id: VCpuId,
        generation: u64,
    ) -> VmBackendResult<bool> {
        self.ensure_owner(vm_id, vcpu_id, generation)?;
        Ok(self.seen.swap(false, Ordering::Relaxed))
    }

    pub(super) fn withdraw(
        &self,
        vm_id: VMId,
        vcpu_id: VCpuId,
        generation: u64,
    ) -> VmBackendResult {
        self.ensure_owner(vm_id, vcpu_id, generation)?;
        self.generation.store(0, Ordering::Release);
        self.seen.store(false, Ordering::Relaxed);
        self.vm_id.store(NO_OWNER, Ordering::Relaxed);
        self.vcpu_id.store(NO_OWNER, Ordering::Relaxed);
        Ok(())
    }

    /// Records an observation for the currently published generation.
    pub(super) fn observe(&self) -> bool {
        if self.generation.load(Ordering::Acquire) == 0 {
            return false;
        }
        self.seen.store(true, Ordering::Relaxed);
        true
    }

    fn ensure_owner(&self, vm_id: VMId, vcpu_id: VCpuId, generation: u64) -> VmBackendResult {
        if generation == 0
            || self.generation.load(Ordering::Acquire) != generation
            || self.vm_id.load(Ordering::Relaxed) != vm_id
            || self.vcpu_id.load(Ordering::Relaxed) != vcpu_id
        {
            Err(VmBackendError::InvalidState)
        } else {
            Ok(())
        }
    }
}

pub(super) fn next_generation(counter: &AtomicU64) -> VmBackendResult<u64> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| VmBackendError::InvalidState)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_generation_consumes_duplicate_observations_once() {
        let publication = MaintenancePublication::new();
        publication.publish(3, 4, 7).unwrap();
        assert!(publication.observe());
        assert!(publication.observe());
        assert_eq!(publication.consume(3, 4, 7), Ok(true));
        assert_eq!(publication.consume(3, 4, 7), Ok(false));
    }

    #[test]
    fn mismatch_and_orphan_are_rejected_without_stealing_ownership() {
        let publication = MaintenancePublication::new();
        assert!(!publication.observe());
        publication.publish(3, 4, 7).unwrap();
        assert_eq!(
            publication.consume(3, 4, 8),
            Err(VmBackendError::InvalidState)
        );
        assert_eq!(
            publication.withdraw(3, 5, 7),
            Err(VmBackendError::InvalidState)
        );
        publication.withdraw(3, 4, 7).unwrap();
        assert!(!publication.observe());
        publication.publish(5, 6, 8).unwrap();
    }

    #[test]
    fn generation_allocation_never_wraps_or_returns_zero() {
        let counter = AtomicU64::new(1);
        assert_eq!(next_generation(&counter), Ok(1));
        assert_eq!(next_generation(&counter), Ok(2));
        let exhausted = AtomicU64::new(u64::MAX);
        assert_eq!(
            next_generation(&exhausted),
            Err(VmBackendError::InvalidState)
        );
    }
}
