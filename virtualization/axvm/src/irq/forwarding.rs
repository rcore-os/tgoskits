//! Host-testable physical interrupt forwarding primitives.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, AtomicU16, AtomicUsize, Ordering};

use ax_kspin::SpinRaw;

use crate::{AxVmError, AxVmResult};

const SETUP_UNINITIALIZED: u8 = 0;
const SETUP_IN_PROGRESS: u8 = 1;
const SETUP_READY: u8 = 2;
const SETUP_FAILED: u8 = 3;
const OWNER_UNCLAIMED: u8 = 0;
const OWNER_RESERVED: u8 = 1;
const OWNER_ACTIVE: u8 = 2;
static NEXT_GENERATION_ID: AtomicUsize = AtomicUsize::new(1);
#[allow(
    dead_code,
    reason = "the controller registry is consumed only by architecture-selected modules"
)]
const UNBOUND_IRQ_DOMAIN: u16 = u16::MAX;

/// One resolved physical-to-guest interrupt route.
#[allow(
    dead_code,
    reason = "physical routes are consumed only by architecture-selected modules"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalIrqRoute {
    host_irq: ax_hal::irq::IrqId,
    guest_irq: usize,
}

#[allow(
    dead_code,
    reason = "physical routes are consumed only by architecture-selected modules"
)]
impl PhysicalIrqRoute {
    pub(crate) const fn new(host_irq: ax_hal::irq::IrqId, guest_irq: usize) -> Self {
        Self {
            host_irq,
            guest_irq,
        }
    }

    pub(crate) const fn host_irq(self) -> ax_hal::irq::IrqId {
        self.host_irq
    }

    pub(crate) const fn guest_irq(self) -> usize {
        self.guest_irq
    }
}

pub(crate) fn next_generation_id() -> usize {
    let id = NEXT_GENERATION_ID.fetch_add(1, Ordering::Relaxed);
    assert_ne!(id, 0, "forwarding generation ID space exhausted");
    id
}
#[allow(
    dead_code,
    reason = "the host-tested mask policy has a production consumer only on AArch64"
)]
pub(crate) fn exclusive_cpu_from_mask(mask: Option<usize>) -> Option<usize> {
    let mask = mask?;
    (mask.count_ones() == 1).then_some(mask.trailing_zeros() as usize)
}

/// An atomic owner table whose releases are scoped to one runtime generation.
#[allow(
    dead_code,
    reason = "the ownership model is consumed only by architecture-selected modules"
)]
pub(crate) struct GenerationOwnerTable<const N: usize> {
    owners: [AtomicUsize; N],
    generations: [AtomicUsize; N],
    states: [AtomicU8; N],
    update_lock: SpinRaw<()>,
}
#[allow(
    dead_code,
    reason = "the ownership model is consumed only by architecture-selected modules"
)]
pub(crate) struct PendingGenerationClaim<'a, const N: usize> {
    table: &'a GenerationOwnerTable<N>,
    owner: usize,
    generation: usize,
    newly_claimed: Vec<usize>,
    committed: bool,
}

#[allow(
    dead_code,
    reason = "the ownership model is consumed only by architecture-selected modules"
)]
impl<const N: usize> PendingGenerationClaim<'_, N> {
    pub(crate) fn commit(mut self) {
        self.table
            .activate_generation(self.owner, self.generation, &self.newly_claimed);
        self.committed = true;
    }
}

impl<const N: usize> Drop for PendingGenerationClaim<'_, N> {
    fn drop(&mut self) {
        if !self.committed {
            self.table
                .release_generation(self.owner, self.generation, &self.newly_claimed);
        }
    }
}

#[allow(
    dead_code,
    reason = "the ownership model is consumed only by architecture-selected modules"
)]
impl<const N: usize> GenerationOwnerTable<N> {
    pub(crate) const fn new() -> Self {
        Self {
            owners: [const { AtomicUsize::new(0) }; N],
            generations: [const { AtomicUsize::new(0) }; N],
            states: [const { AtomicU8::new(OWNER_UNCLAIMED) }; N],
            update_lock: SpinRaw::new(()),
        }
    }

    pub(crate) fn claim_all(
        &self,
        owner: usize,
        generation: usize,
        indices: &[usize],
    ) -> Result<PendingGenerationClaim<'_, N>, usize> {
        assert_ne!(owner, 0, "zero is reserved for an unowned IRQ");
        assert_ne!(generation, 0, "zero is reserved for no runtime generation");
        let _guard = self.update_lock.lock();

        if let Some(&conflict) = indices.iter().find(|&&index| {
            let current_owner = self.owners[index].load(Ordering::Acquire);
            let current_generation = self.generations[index].load(Ordering::Acquire);
            current_owner != 0 && (current_owner != owner || current_generation != generation)
        }) {
            return Err(conflict);
        }

        let mut newly_claimed = Vec::new();
        for &index in indices {
            if self.owners[index].load(Ordering::Acquire) == 0 {
                self.generations[index].store(generation, Ordering::Release);
                self.owners[index].store(owner, Ordering::Release);
                self.states[index].store(OWNER_RESERVED, Ordering::Release);
                newly_claimed.push(index);
            }
        }
        Ok(PendingGenerationClaim {
            table: self,
            owner,
            generation,
            newly_claimed,
            committed: false,
        })
    }

    pub(crate) fn release_generation(&self, owner: usize, generation: usize, indices: &[usize]) {
        let _guard = self.update_lock.lock();
        for &index in indices {
            if self.owners[index].load(Ordering::Acquire) == owner
                && self.generations[index].load(Ordering::Acquire) == generation
            {
                self.states[index].store(OWNER_UNCLAIMED, Ordering::Release);
                self.owners[index].store(0, Ordering::Release);
                self.generations[index].store(0, Ordering::Release);
            }
        }
    }

    pub(crate) fn is_owned_by(&self, index: usize, owner: usize) -> bool {
        self.states[index].load(Ordering::Acquire) == OWNER_ACTIVE
            && self.owners[index].load(Ordering::Acquire) == owner
    }

    pub(crate) fn is_active_owner(&self, index: usize, owner: usize, generation: usize) -> bool {
        self.states[index].load(Ordering::Acquire) == OWNER_ACTIVE
            && self.owners[index].load(Ordering::Acquire) == owner
            && self.generations[index].load(Ordering::Acquire) == generation
    }

    fn activate_generation(&self, owner: usize, generation: usize, indices: &[usize]) {
        let _guard = self.update_lock.lock();
        for &index in indices {
            assert_eq!(self.owners[index].load(Ordering::Acquire), owner);
            assert_eq!(self.generations[index].load(Ordering::Acquire), generation);
            self.states[index].store(OWNER_ACTIVE, Ordering::Release);
        }
    }
}

/// Generation ownership for one bounded interrupt-controller domain.
#[allow(
    dead_code,
    reason = "the controller registry is consumed only by architecture-selected modules"
)]
pub(crate) struct ControllerIrqRegistry<const N: usize> {
    domain: AtomicU16,
    first_hwirq: u32,
    owners: GenerationOwnerTable<N>,
}

#[allow(
    dead_code,
    reason = "the controller registry is consumed only by architecture-selected modules"
)]
impl<const N: usize> ControllerIrqRegistry<N> {
    pub(crate) const fn new(first_hwirq: u32) -> Self {
        Self {
            domain: AtomicU16::new(UNBOUND_IRQ_DOMAIN),
            first_hwirq,
            owners: GenerationOwnerTable::new(),
        }
    }

    pub(crate) fn bind_domain(
        &self,
        domain: ax_hal::irq::IrqDomainId,
    ) -> Result<(), ax_hal::irq::IrqDomainId> {
        match self.domain.compare_exchange(
            UNBOUND_IRQ_DOMAIN,
            domain.0,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(current) if current == domain.0 => Ok(()),
            Err(current) => Err(ax_hal::irq::IrqDomainId(current)),
        }
    }

    pub(crate) fn slot(&self, irq: ax_hal::irq::IrqId) -> Option<usize> {
        if self.domain.load(Ordering::Acquire) != irq.domain.0 {
            return None;
        }
        let slot = irq.hwirq.0.checked_sub(self.first_hwirq)? as usize;
        (slot < N).then_some(slot)
    }

    pub(crate) fn bound_irq(&self, hwirq: u32) -> Option<ax_hal::irq::IrqId> {
        let domain = self.domain.load(Ordering::Acquire);
        if domain == UNBOUND_IRQ_DOMAIN {
            return None;
        }
        let irq =
            ax_hal::irq::IrqId::new(ax_hal::irq::IrqDomainId(domain), ax_hal::irq::HwIrq(hwirq));
        self.slot(irq).map(|_| irq)
    }

    pub(crate) fn claim_all(
        &self,
        owner: usize,
        generation: usize,
        routes: &[PhysicalIrqRoute],
    ) -> Result<PendingGenerationClaim<'_, N>, PhysicalIrqRoute> {
        let indices = routes
            .iter()
            .map(|route| self.slot(route.host_irq).ok_or(*route))
            .collect::<Result<Vec<_>, _>>()?;
        self.owners
            .claim_all(owner, generation, &indices)
            .map_err(|conflict| {
                routes
                    .iter()
                    .copied()
                    .find(|route| self.slot(route.host_irq) == Some(conflict))
                    .expect("a conflicting slot came from the validated route set")
            })
    }

    pub(crate) fn release_generation(
        &self,
        owner: usize,
        generation: usize,
        routes: &[PhysicalIrqRoute],
    ) {
        let indices = routes
            .iter()
            .filter_map(|route| self.slot(route.host_irq))
            .collect::<Vec<_>>();
        self.owners.release_generation(owner, generation, &indices);
    }

    pub(crate) fn is_active_owner(
        &self,
        irq: ax_hal::irq::IrqId,
        owner: usize,
        generation: usize,
    ) -> bool {
        self.slot(irq)
            .is_some_and(|slot| self.owners.is_active_owner(slot, owner, generation))
    }

    pub(crate) fn active_claim(&self, irq: ax_hal::irq::IrqId) -> Option<(usize, usize)> {
        let slot = self.slot(irq)?;
        let _guard = self.owners.update_lock.lock();
        if self.owners.states[slot].load(Ordering::Acquire) != OWNER_ACTIVE {
            return None;
        }
        let owner = self.owners.owners[slot].load(Ordering::Acquire);
        let generation = self.owners.generations[slot].load(Ordering::Acquire);
        Some((owner, generation))
    }
}

/// Resolves an exclusively owned IRQ while preventing injection into another
/// VM's active vCPU context.
#[allow(
    dead_code,
    reason = "the host-tested owner policy has a production consumer only on AArch64"
)]
pub(crate) fn resolve_exclusive_irq_owner(
    active_claim: Option<(usize, usize)>,
    current_claim: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let active_claim = active_claim?;
    match current_claim {
        Some(current_claim) if current_claim != active_claim => None,
        _ => Some(active_claim),
    }
}

/// One forwarding-setup generation, owned by one VM runtime generation.
pub(crate) struct ForwardingSetup {
    state: AtomicU8,
    failure: SpinRaw<Option<AxVmError>>,
}

impl ForwardingSetup {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU8::new(SETUP_UNINITIALIZED),
            failure: SpinRaw::new(None),
        }
    }

    /// Runs setup once and publishes its result to every concurrent caller.
    pub(crate) fn run_once(&self, setup: impl FnOnce() -> AxVmResult) -> AxVmResult {
        let mut setup = Some(setup);
        loop {
            match self.state.load(Ordering::Acquire) {
                SETUP_UNINITIALIZED => {
                    if self
                        .state
                        .compare_exchange(
                            SETUP_UNINITIALIZED,
                            SETUP_IN_PROGRESS,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        continue;
                    }
                    let result = setup.take().expect("setup closure is consumed once")();
                    match &result {
                        Ok(()) => self.state.store(SETUP_READY, Ordering::Release),
                        Err(error) => {
                            *self.failure.lock() = Some(error.clone());
                            self.state.store(SETUP_FAILED, Ordering::Release);
                        }
                    }
                    return result;
                }
                SETUP_IN_PROGRESS => core::hint::spin_loop(),
                SETUP_READY => return Ok(()),
                SETUP_FAILED => {
                    return Err(self
                        .failure
                        .lock()
                        .clone()
                        .expect("failed setup publishes its error before the state"));
                }
                _ => unreachable!("forwarding setup state is internal"),
            }
        }
    }
}

/// Relevant state of one GIC list register.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LrState {
    Invalid,
    Pending,
    Active,
    PendingActive,
}

/// Architecture-neutral snapshot used to match an existing list register.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LrSnapshot {
    pub virtual_intid: usize,
    pub hardware: bool,
    pub physical_intid: Option<usize>,
    pub state: LrState,
}

/// Requested virtual-to-physical list-register route.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LrRouteRequest {
    pub virtual_intid: usize,
    pub physical_intid: Option<usize>,
}

#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExistingLrAction {
    Coalesce,
    MarkPendingActive,
}

/// Returns the current-CPU hardware route for the AArch64 virtual timer PPI.
#[allow(
    dead_code,
    reason = "the AArch64 virtual-timer route has a production consumer only on AArch64"
)]
pub(crate) fn aarch64_virtual_timer_route(intid: usize) -> Option<LrRouteRequest> {
    (intid == 27).then_some(LrRouteRequest {
        virtual_intid: 27,
        physical_intid: Some(27),
    })
}

/// Returns whether a live list register already represents the exact request.
#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
pub(crate) fn lr_matches_route(snapshot: LrSnapshot, request: LrRouteRequest) -> bool {
    let live = matches!(
        snapshot.state,
        LrState::Pending | LrState::Active | LrState::PendingActive
    );
    let expected_hardware = request.physical_intid.is_some();
    live && snapshot.virtual_intid == request.virtual_intid
        && snapshot.hardware == expected_hardware
        && snapshot.physical_intid == request.physical_intid
}

#[allow(
    dead_code,
    reason = "the host-testable LR model has a production consumer only on AArch64"
)]
pub(crate) fn existing_lr_action(
    snapshot: LrSnapshot,
    request: LrRouteRequest,
) -> Option<ExistingLrAction> {
    if !lr_matches_route(snapshot, request) {
        return None;
    }
    if snapshot.state == LrState::Active && request.physical_intid.is_none() {
        return Some(ExistingLrAction::MarkPendingActive);
    }
    Some(ExistingLrAction::Coalesce)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    use super::{
        ExistingLrAction, LrRouteRequest, LrSnapshot, LrState, aarch64_virtual_timer_route,
        existing_lr_action, lr_matches_route,
    };
    use crate::AxVmError;

    fn irq(domain: u16, hwirq: u32) -> ax_hal::irq::IrqId {
        ax_hal::irq::IrqId::new(ax_hal::irq::IrqDomainId(domain), ax_hal::irq::HwIrq(hwirq))
    }

    #[test]
    fn controller_registry_rejects_equal_hwirq_from_another_domain() {
        let registry = super::ControllerIrqRegistry::<32>::new(1);
        let route = super::PhysicalIrqRoute::new(irq(7, 5), 9);
        let wrong_domain = super::PhysicalIrqRoute::new(irq(8, 5), 9);

        registry.bind_domain(route.host_irq().domain).unwrap();

        assert_eq!(registry.slot(route.host_irq()), Some(4));
        assert_eq!(registry.slot(wrong_domain.host_irq()), None);
    }

    #[test]
    fn controller_registry_checks_owner_and_generation_for_typed_irq() {
        let registry = super::ControllerIrqRegistry::<32>::new(1);
        let route = super::PhysicalIrqRoute::new(irq(7, 5), 9);
        registry.bind_domain(route.host_irq().domain).unwrap();
        registry
            .claim_all(3, 10, core::slice::from_ref(&route))
            .unwrap()
            .commit();

        assert!(registry.is_active_owner(route.host_irq(), 3, 10));
        assert!(!registry.is_active_owner(route.host_irq(), 3, 11));
        assert!(!registry.is_active_owner(irq(8, 5), 3, 10));
    }

    #[test]
    fn exclusive_irq_owner_survives_a_temporary_absence_of_vcpu_context() {
        let registry = super::ControllerIrqRegistry::<32>::new(1);
        let route = super::PhysicalIrqRoute::new(irq(7, 5), 9);
        registry.bind_domain(route.host_irq().domain).unwrap();
        registry
            .claim_all(3, 10, core::slice::from_ref(&route))
            .unwrap()
            .commit();

        assert_eq!(registry.active_claim(route.host_irq()), Some((3, 10)));
        assert_eq!(
            super::resolve_exclusive_irq_owner(Some((3, 10)), None),
            Some((3, 10))
        );
        assert_eq!(
            super::resolve_exclusive_irq_owner(Some((3, 10)), Some((4, 11))),
            None
        );
    }

    #[test]
    fn generation_claim_same_owner_different_generation_conflicts_without_claiming_free_prefix() {
        let owners = super::GenerationOwnerTable::<8>::new();
        owners.claim_all(3, 10, &[1]).unwrap().commit();

        let conflict = match owners.claim_all(3, 11, &[1, 2]) {
            Err(index) => index,
            Ok(_) => panic!("a different generation must conflict"),
        };

        assert_eq!(conflict, 1);
        assert!(!owners.is_owned_by(2, 3));
    }

    #[test]
    fn generation_claim_late_old_release_keeps_new_generation() {
        let owners = super::GenerationOwnerTable::<8>::new();
        owners.claim_all(3, 10, &[1]).unwrap().commit();
        owners.release_generation(3, 10, &[1]);
        owners.claim_all(3, 11, &[1]).unwrap().commit();

        owners.release_generation(3, 10, &[1]);
        assert!(owners.is_owned_by(1, 3));

        owners.release_generation(3, 11, &[1]);
        assert!(!owners.is_owned_by(1, 3));
    }

    #[test]
    fn generation_claim_uncommitted_guard_rolls_back_on_drop() {
        let owners = super::GenerationOwnerTable::<8>::new();
        let claim = owners.claim_all(3, 10, &[1, 2]).unwrap();

        drop(claim);

        assert!(!owners.is_owned_by(1, 3));
        assert!(!owners.is_owned_by(2, 3));
    }

    #[test]
    fn generation_claim_committed_guard_survives_drop() {
        let owners = super::GenerationOwnerTable::<8>::new();
        owners.claim_all(3, 10, &[1]).unwrap().commit();

        assert!(owners.is_owned_by(1, 3));
    }

    #[test]
    fn generation_claim_is_invisible_until_committed() {
        let owners = super::GenerationOwnerTable::<8>::new();
        let claim = owners.claim_all(3, 10, &[1]).unwrap();

        assert!(!owners.is_active_owner(1, 3, 10));

        claim.commit();
        assert!(owners.is_active_owner(1, 3, 10));
    }

    #[test]
    fn active_ownership_requires_the_exact_runtime_generation() {
        let owners = super::GenerationOwnerTable::<8>::new();
        owners.claim_all(3, 10, &[1]).unwrap().commit();

        assert!(owners.is_active_owner(1, 3, 10));
        assert!(!owners.is_active_owner(1, 3, 11));
    }

    #[test]
    fn generation_claim_conflicting_batch_does_not_claim_its_free_prefix() {
        let owners = super::GenerationOwnerTable::<8>::new();
        owners.claim_all(1, 1, &[4]).unwrap().commit();

        let conflict = match owners.claim_all(2, 2, &[3, 4]) {
            Err(index) => index,
            Ok(_) => panic!("another owner must conflict"),
        };

        assert_eq!(conflict, 4);
        assert!(!owners.is_owned_by(3, 2));
        assert!(owners.is_owned_by(4, 1));
    }

    #[test]
    fn exclusive_cpu_from_mask_rejects_none_or_empty() {
        assert_eq!(super::exclusive_cpu_from_mask(None), None);
        assert_eq!(super::exclusive_cpu_from_mask(Some(0)), None);
    }

    #[test]
    fn exclusive_cpu_from_mask_rejects_multiple_bits() {
        assert_eq!(super::exclusive_cpu_from_mask(Some(0b1010)), None);
    }

    #[test]
    fn exclusive_cpu_from_mask_accepts_one_bit() {
        assert_eq!(super::exclusive_cpu_from_mask(Some(0b1000)), Some(3));
    }

    #[test]
    fn forwarding_setup_runs_once_for_two_callers() {
        let setup = Arc::new(super::ForwardingSetup::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(3));
        let callers = (0..2)
            .map(|_| {
                let setup = setup.clone();
                let calls = calls.clone();
                let start = start.clone();
                thread::spawn(move || {
                    start.wait();
                    setup.run_once(|| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    })
                })
            })
            .collect::<alloc::vec::Vec<_>>();

        start.wait();
        for caller in callers {
            caller.join().unwrap().unwrap();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_setup_is_local_to_one_runtime_generation() {
        let first = crate::vm::VmRuntimeHandle::new();
        let second = crate::vm::VmRuntimeHandle::new();
        let failure = AxVmError::Unsupported {
            operation: "test forwarding",
            detail: "failed generation".into(),
        };

        assert_eq!(
            first.run_forwarding_setup_once(|| Err(failure.clone())),
            Err(failure.clone())
        );
        assert_eq!(first.run_forwarding_setup_once(|| Ok(())), Err(failure));
        assert!(second.run_forwarding_setup_once(|| Ok(())).is_ok());
        assert_ne!(
            first.forwarding_generation_id(),
            second.forwarding_generation_id()
        );
    }

    #[test]
    fn lr_matches_only_identical_route_and_live_state() {
        let request = LrRouteRequest {
            virtual_intid: 45,
            physical_intid: Some(45),
        };
        let live = LrSnapshot {
            virtual_intid: 45,
            hardware: true,
            physical_intid: Some(45),
            state: LrState::Pending,
        };

        assert!(lr_matches_route(live, request));
        assert!(lr_matches_route(
            LrSnapshot {
                state: LrState::Active,
                ..live
            },
            request
        ));
        assert!(lr_matches_route(
            LrSnapshot {
                state: LrState::PendingActive,
                ..live
            },
            request,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                hardware: false,
                ..live
            },
            request
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                physical_intid: Some(46),
                ..live
            },
            request,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                virtual_intid: 46,
                ..live
            },
            request,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                state: LrState::Invalid,
                ..live
            },
            request
        ));

        let software = LrRouteRequest {
            virtual_intid: 27,
            physical_intid: None,
        };
        assert!(lr_matches_route(
            LrSnapshot {
                virtual_intid: 27,
                hardware: false,
                physical_intid: None,
                state: LrState::Pending,
            },
            software,
        ));
        assert!(!lr_matches_route(
            LrSnapshot {
                virtual_intid: 27,
                hardware: true,
                physical_intid: Some(27),
                state: LrState::Pending,
            },
            software,
        ));
    }

    #[test]
    fn active_software_irq_reinjection_marks_the_lr_pending_active() {
        let request = LrRouteRequest {
            virtual_intid: 33,
            physical_intid: None,
        };
        let active = LrSnapshot {
            virtual_intid: 33,
            hardware: false,
            physical_intid: None,
            state: LrState::Active,
        };

        assert_eq!(
            existing_lr_action(active, request),
            Some(ExistingLrAction::MarkPendingActive)
        );
    }

    #[test]
    fn active_hardware_irq_reinjection_remains_coalesced() {
        let request = LrRouteRequest {
            virtual_intid: 45,
            physical_intid: Some(45),
        };
        let active = LrSnapshot {
            virtual_intid: 45,
            hardware: true,
            physical_intid: Some(45),
            state: LrState::Active,
        };

        assert_eq!(
            existing_lr_action(active, request),
            Some(ExistingLrAction::Coalesce)
        );
    }

    #[test]
    fn aarch64_virtual_timer_uses_a_current_cpu_hardware_route() {
        assert_eq!(
            aarch64_virtual_timer_route(27),
            Some(LrRouteRequest {
                virtual_intid: 27,
                physical_intid: Some(27),
            })
        );
        assert_eq!(aarch64_virtual_timer_route(26), None);
        assert_eq!(aarch64_virtual_timer_route(30), None);
    }
}
