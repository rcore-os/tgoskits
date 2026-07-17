// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! RISC-V virtual PLIC interrupt backend.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_cpu_local::CpuPin;
use ax_std::os::arceos::task::{ThreadWakeHandle, WakeResult};
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerError,
    DeviceManagerResult, DeviceRegistration, MmioDeviceAdapter,
};
use axdevice_base::{IrqError, IrqLineId, IrqResult, IrqSink};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, VMId, VMInterruptMode};
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE,
    PLIC_NUM_SOURCES, VPlicGlobal,
};
use spin::Once;

#[cfg(any(feature = "fs", feature = "host-fs"))]
use super::route_transaction::{RouteRevocation, current_route_identity, revoke_active_route};
use super::{
    completion_restore::{restore_all, restore_present_suffix},
    forwarded_ingress::{FORWARDED_IRQ_DRAIN_BATCH, ForwardedIrqIngress, ForwardedIrqPublish},
    owner_doorbell::{FixedOwnerContext, OwnerDoorbell},
    route_transaction::{
        ROUTE_GENERATION_MAX, RouteActivation, RouteControl, RoutePreparation,
        RouteReservationError, RouteTransactionState, activate_published_route,
        prepare_route_if_available,
    },
};
use crate::{
    AxVmError, AxVmResult, ax_err, ax_err_type,
    irq::{
        InterruptFabric, RiscvPhysicalIrqClaim, RiscvPlatformIrq, RiscvPlatformIrqRouteResult,
        RiscvPlatformIrqRouteStatus,
    },
};

static PLATFORM_VPLIC_ROUTE: PlatformVplicRouteSlot = PlatformVplicRouteSlot::new();
// Hard IRQs consume only the immutable published route and never acquire this
// short control-plane lock. The complete canonical key and generation reserve
// ownership while platform preparation and activation run outside the lock.
type PlatformVplicRouteState = RouteTransactionState<PlatformVplicRouteKey>;
static PLATFORM_VPLIC_ROUTE_CONTROL: RouteControl<PlatformVplicRouteState> =
    RouteControl::new(PlatformVplicRouteState::new());
pub(crate) const FORWARDED_COMPLETION_DRAIN_BATCH: usize = 64;
const PLATFORM_VPLIC_SOURCE_WORDS: usize = PLIC_NUM_SOURCES.div_ceil(u64::BITS as usize);
const PLATFORM_VPLIC_ROUTE_MAX_SOURCES: usize =
    ax_plat::irq::riscv64_hv::RISCV_PLIC_GUEST_ROUTE_MAX_SOURCES;
const ROUTE_PHASE_MASK: u64 = 0b11;
const ROUTE_VACANT: u64 = 0;
const ROUTE_ACTIVE: u64 = 1;
const ROUTE_REVOKING: u64 = 2;

struct PlatformVplicRouteSlot {
    state: AtomicU64,
    publishers: AtomicUsize,
    platform_released: AtomicBool,
    route: UnsafeCell<MaybeUninit<PlatformVplicRoute>>,
}

// SAFETY: route storage is written only in Vacant with no publishers. IRQ
// readers increment and revalidate the exact Active generation before taking
// a reference. Revocation publishes Revoking and drains readers before drop.
unsafe impl Sync for PlatformVplicRouteSlot {}

impl PlatformVplicRouteSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU64::new(ROUTE_VACANT),
            publishers: AtomicUsize::new(0),
            platform_released: AtomicBool::new(false),
            route: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    fn install(&self, route: PlatformVplicRoute, generation: u64) {
        assert_eq!(self.state.load(Ordering::Acquire), ROUTE_VACANT);
        assert_eq!(self.publishers.load(Ordering::Acquire), 0);
        assert!(!self.platform_released.load(Ordering::Acquire));
        // SAFETY: Vacant and zero publishers grant the preparation permit
        // exclusive access until Active is published below.
        unsafe { (*self.route.get()).write(route) };
        self.state
            .store(route_state(generation, ROUTE_ACTIVE), Ordering::Release);
    }

    fn acquire_irq(&self) -> PlatformRouteAcquire<'_> {
        let observed = self.state.load(Ordering::Acquire);
        match observed & ROUTE_PHASE_MASK {
            ROUTE_VACANT => PlatformRouteAcquire::Vacant,
            ROUTE_REVOKING => PlatformRouteAcquire::Quarantined,
            ROUTE_ACTIVE => {
                self.publishers.fetch_add(1, Ordering::AcqRel);
                if self.state.load(Ordering::Acquire) != observed {
                    self.publishers.fetch_sub(1, Ordering::Release);
                    return PlatformRouteAcquire::Quarantined;
                }
                // SAFETY: exact Active generation was revalidated after the
                // publisher increment; Revoking cannot drop until it drains.
                PlatformRouteAcquire::Active(PlatformRoutePublisher {
                    slot: self,
                    route: unsafe { self.route_ref() },
                })
            }
            _ => unreachable!(),
        }
    }

    fn acquire_control(&self, generation: u64) -> Option<PlatformRoutePublisher<'_>> {
        let observed = self.state.load(Ordering::Acquire);
        if observed != route_state(generation, ROUTE_ACTIVE)
            && observed != route_state(generation, ROUTE_REVOKING)
        {
            return None;
        }
        self.publishers.fetch_add(1, Ordering::AcqRel);
        if self.state.load(Ordering::Acquire) != observed {
            self.publishers.fetch_sub(1, Ordering::Release);
            return None;
        }
        // SAFETY: the exact initialized generation was revalidated and cannot
        // be cleared until this task-context publisher is dropped.
        Some(PlatformRoutePublisher {
            slot: self,
            route: unsafe { self.route_ref() },
        })
    }

    fn acquire_active_control(&self) -> Option<PlatformRoutePublisher<'_>> {
        let observed = self.state.load(Ordering::Acquire);
        if observed & ROUTE_PHASE_MASK != ROUTE_ACTIVE {
            return None;
        }
        self.acquire_control(observed >> 2)
    }

    fn begin_revocation(&self, generation: u64) {
        let active = route_state(generation, ROUTE_ACTIVE);
        let revoking = route_state(generation, ROUTE_REVOKING);
        match self
            .state
            .compare_exchange(active, revoking, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {}
            Err(observed) => assert_eq!(
                observed, revoking,
                "RISC-V AxVM route revocation observed another generation"
            ),
        }
    }

    fn publishers_drained(&self, generation: u64) -> bool {
        self.state.load(Ordering::Acquire) == route_state(generation, ROUTE_REVOKING)
            && self.publishers.load(Ordering::Acquire) == 0
    }

    fn platform_released(&self, generation: u64) -> bool {
        assert_eq!(
            self.state.load(Ordering::Acquire),
            route_state(generation, ROUTE_REVOKING)
        );
        self.platform_released.load(Ordering::Acquire)
    }

    fn mark_platform_released(&self, generation: u64) {
        assert_eq!(
            self.state.load(Ordering::Acquire),
            route_state(generation, ROUTE_REVOKING)
        );
        self.platform_released.store(true, Ordering::Release);
    }

    fn finish_revocation(&self, generation: u64) {
        assert!(self.publishers_drained(generation));
        assert!(self.platform_released.load(Ordering::Acquire));
        // SAFETY: the platform lease is gone, Revoking blocks IRQ readers,
        // and the final publisher has drained.
        unsafe { (*self.route.get()).assume_init_drop() };
        self.platform_released.store(false, Ordering::Release);
        self.state.store(ROUTE_VACANT, Ordering::Release);
    }

    unsafe fn route_ref(&self) -> &PlatformVplicRoute {
        // SAFETY: every caller proves a published non-Vacant lifecycle state.
        unsafe { (&*self.route.get()).assume_init_ref() }
    }
}

struct PlatformRoutePublisher<'a> {
    slot: &'a PlatformVplicRouteSlot,
    route: &'a PlatformVplicRoute,
}

impl Drop for PlatformRoutePublisher<'_> {
    fn drop(&mut self) {
        self.slot.publishers.fetch_sub(1, Ordering::Release);
    }
}

enum PlatformRouteAcquire<'a> {
    Vacant,
    Active(PlatformRoutePublisher<'a>),
    Quarantined,
}

const fn route_state(generation: u64, phase: u64) -> u64 {
    assert!(generation != 0 && generation <= ROUTE_GENERATION_MAX);
    generation << 2 | phase
}

struct PlatformVplicRoute {
    binding: VplicVcpuBinding,
    target_cpu: usize,
    irq_sources: Box<[u32]>,
}

impl PlatformVplicRoute {
    fn new(binding: VplicVcpuBinding, target_cpu: usize, irq_sources: &[u32]) -> AxVmResult<Self> {
        if irq_sources.is_empty() || irq_sources.len() > PLATFORM_VPLIC_ROUTE_MAX_SOURCES {
            return Err(AxVmError::invalid_config(format_args!(
                "RISC-V passthrough route requires 1..={PLATFORM_VPLIC_ROUTE_MAX_SOURCES} sources"
            )));
        }
        let mut canonical_sources = irq_sources.to_vec();
        canonical_sources.sort_unstable();
        if canonical_sources
            .iter()
            .any(|source| *source == 0 || *source as usize >= PLIC_NUM_SOURCES)
            || canonical_sources
                .windows(2)
                .any(|sources| sources[0] == sources[1])
        {
            return Err(AxVmError::invalid_config(
                "RISC-V passthrough IRQ sources must be unique, nonzero PLIC source IDs",
            ));
        }
        Ok(Self {
            binding,
            target_cpu,
            irq_sources: canonical_sources.into_boxed_slice(),
        })
    }

    fn same_route(&self, installed: &Self) -> bool {
        self.target_cpu == installed.target_cpu
            && self.irq_sources == installed.irq_sources
            && self.binding.same_binding(&installed.binding)
    }

    fn contains_source(&self, source: u32) -> bool {
        self.irq_sources.binary_search(&source).is_ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlatformVplicRouteKey {
    vm_id: VMId,
    context_id: usize,
    target_cpu: usize,
    vplic: usize,
    notifications: usize,
    irq_sources: [u64; PLATFORM_VPLIC_SOURCE_WORDS],
}

impl PlatformVplicRouteKey {
    fn from_route(route: &PlatformVplicRoute) -> Self {
        let mut irq_sources = [0; PLATFORM_VPLIC_SOURCE_WORDS];
        for &source in route.irq_sources.iter() {
            let source = source as usize;
            irq_sources[source / u64::BITS as usize] |= 1 << (source % u64::BITS as usize);
        }
        Self {
            vm_id: route.binding.vm_id,
            context_id: route.binding.context_id,
            target_cpu: route.target_cpu,
            vplic: Arc::as_ptr(&route.binding.vplic) as usize,
            notifications: Arc::as_ptr(&route.binding.notifications) as usize,
            irq_sources,
        }
    }
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
struct PlatformRouteRevocationSnapshot {
    binding: VplicVcpuBinding,
    target_cpu: usize,
    irq_sources: [u32; PLATFORM_VPLIC_ROUTE_MAX_SOURCES],
    source_count: usize,
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
impl PlatformRouteRevocationSnapshot {
    fn from_publisher(publisher: &PlatformRoutePublisher<'_>) -> Self {
        let route = publisher.route;
        let mut irq_sources = [0; PLATFORM_VPLIC_ROUTE_MAX_SOURCES];
        irq_sources[..route.irq_sources.len()].copy_from_slice(&route.irq_sources);
        Self {
            binding: route.binding.clone(),
            target_cpu: route.target_cpu,
            irq_sources,
            source_count: route.irq_sources.len(),
        }
    }

    fn sources(&self) -> &[u32] {
        &self.irq_sources[..self.source_count]
    }
}

pub(crate) struct ForwardedCompletionBatch {
    claims: [Option<RiscvPhysicalIrqClaim>; FORWARDED_COMPLETION_DRAIN_BATCH],
    len: usize,
}

impl ForwardedCompletionBatch {
    const fn empty() -> Self {
        Self {
            claims: [None; FORWARDED_COMPLETION_DRAIN_BATCH],
            len: 0,
        }
    }

    pub(crate) fn claims(&self) -> &[Option<RiscvPhysicalIrqClaim>] {
        &self.claims[..self.len]
    }
}

pub(crate) struct RiscvInterruptResources {
    pub(crate) interrupt_fabric: InterruptFabric,
    pub(crate) vplic: Option<VplicResources>,
}

#[derive(Clone)]
pub(crate) struct VplicResources {
    vm_id: VMId,
    vplic: Arc<VPlicGlobal>,
    notifications: Arc<VplicNotifications>,
}

#[derive(Clone)]
pub(crate) struct VplicVcpuBinding {
    vm_id: VMId,
    vplic: Arc<VPlicGlobal>,
    notifications: Arc<VplicNotifications>,
    context_id: usize,
}

impl VplicVcpuBinding {
    pub(crate) fn install_wake_target(&self, wake: ThreadWakeHandle) {
        self.notifications.install(self.context_id, wake);
    }

    pub(crate) fn install_platform_route(
        &self,
        cpu_id: usize,
        irq_sources: &[u32],
        cpu_pin: &CpuPin,
    ) -> AxVmResult<RiscvPlatformIrqRouteResult> {
        let pinned_cpu = ax_percpu::bound_current(cpu_pin)
            .map_err(|error| {
                AxVmError::resource_unavailable(
                    "RISC-V platform IRQ route",
                    format_args!("the pinned CPU area is not bound: {error}"),
                )
            })?
            .cpu_index()
            .as_usize();
        if pinned_cpu != cpu_id {
            return Err(AxVmError::resource_conflict(
                "RISC-V platform IRQ route",
                format_args!("route target CPU {cpu_id} does not match pinned CPU {pinned_cpu}"),
            ));
        }
        if !self.notifications.has_owner(self.context_id) {
            return Err(AxVmError::resource_unavailable(
                "RISC-V platform IRQ route",
                "the fixed vPLIC owner has no stable scheduler wake target",
            ));
        }
        if let Some(owner) = self.notifications.platform_owner_context()
            && owner != self.context_id
        {
            return Err(AxVmError::resource_conflict(
                "RISC-V platform IRQ route",
                format_args!(
                    "vPLIC context {owner} already owns the monitor-wide passthrough route"
                ),
            ));
        }

        let candidate = PlatformVplicRoute::new(self.clone(), cpu_id, irq_sources)?;
        let route_key = PlatformVplicRouteKey::from_route(&candidate);
        let preparation = prepare_route_if_available(&PLATFORM_VPLIC_ROUTE_CONTROL, route_key)
            .map_err(map_route_reservation_error)?;
        let RoutePreparation::Reserved(mut preparation_permit) = preparation else {
            let installed = PLATFORM_VPLIC_ROUTE
                .acquire_active_control()
                .ok_or_else(|| {
                    AxVmError::resource_unavailable(
                        "RISC-V platform IRQ route",
                        "the matching route entered revocation during lookup",
                    )
                })?;
            assert!(candidate.same_route(installed.route) && self.is_platform_owner());
            return Ok(RiscvPlatformIrqRouteResult {
                status: RiscvPlatformIrqRouteStatus::Activated,
                source: 0,
            });
        };
        let prepared = RiscvPlatformIrq::prepare_virtual_irq_targets(cpu_id, irq_sources, cpu_pin);
        if !prepared.is_prepared() {
            assert!(
                !prepared.is_activated(),
                "the platform activated a RISC-V route before AxVM published its owner"
            );
            return Ok(prepared);
        }
        // The lower layer now owns permanent physical leases. Any subsequent
        // invariant failure must quarantine this generation as reserved
        // instead of exposing a false vacant state to another owner.
        let route_generation = preparation_permit.generation();
        preparation_permit.begin_irreversible();

        assert!(
            self.notifications.install_platform_owner(self.context_id),
            "prepared RISC-V platform IRQ route conflicts with the vPLIC owner"
        );
        PLATFORM_VPLIC_ROUTE.install(candidate, route_generation);
        assert!(
            self.notifications
                .activate_platform_owner(self.context_id, route_generation)
        );
        preparation_permit.publish();

        let activation = activate_published_route(&PLATFORM_VPLIC_ROUTE_CONTROL, route_key)
            .expect("a newly published RISC-V route must reserve activation");
        let RouteActivation::Reserved(mut activation_permit) = activation else {
            panic!("a newly published RISC-V route cannot already be active");
        };
        // Platform activation is an infallible commit after both layers have
        // validated the same pinned owner key. An unexpected failure is fatal
        // quarantine, never a rollback to a retryable published route.
        activation_permit.begin_irreversible();
        let activated =
            RiscvPlatformIrq::activate_virtual_irq_targets(cpu_id, irq_sources, cpu_pin);
        assert!(
            activated.is_activated(),
            "prepared RISC-V platform IRQ route could not be activated: status={:?}, source={}",
            activated.status,
            activated.source
        );
        activation_permit.finish();
        Ok(activated)
    }

    fn same_binding(&self, installed: &Self) -> bool {
        installed.vm_id == self.vm_id
            && installed.context_id == self.context_id
            && Arc::ptr_eq(&installed.vplic, &self.vplic)
            && Arc::ptr_eq(&installed.notifications, &self.notifications)
    }

    pub(crate) fn is_platform_owner(&self) -> bool {
        self.notifications.is_platform_owner(self.context_id)
            && self.notifications.accepts_forwarded_irqs()
    }

    pub(crate) fn take_line_level(&self) -> Result<bool, riscv_vplic::VplicError> {
        self.vplic
            .take_context_notification(self.context_id)?
            .map_or_else(|| self.vplic.context_line_asserted(self.context_id), Ok)
    }

    pub(crate) fn forward_physical_irq(&self, claim: RiscvPhysicalIrqClaim) -> bool {
        if !self.notifications.accepts_forwarded_irqs() {
            // Revocation consumes already-claimed old-generation IRQs without
            // publishing them to either the guest or a host handler.
            return true;
        }
        if !self.is_platform_owner() {
            return false;
        }
        let source = claim.source() as usize;
        let encoded = encode_claim(claim);
        match self.notifications.ingress.publish(source, encoded) {
            ForwardedIrqPublish::WakeOwner => {
                if !self.notifications.publish_owner(self.context_id) {
                    self.notifications.ingress.record_fault();
                    if self.notifications.ingress.retry_after_failed_wake()
                        && !self.notifications.publish_owner(self.context_id)
                    {
                        // The route was activated only after installing this
                        // stable wake target. A second failure is therefore an
                        // invariant fault; retain the masked claim fail-closed.
                        self.notifications.ingress.record_fault();
                    }
                }
            }
            ForwardedIrqPublish::Coalesced => {}
            ForwardedIrqPublish::Fault => return false,
        }
        true
    }

    /// Takes one bounded completion batch in normal task context.
    ///
    /// Only the fixed platform owner may consume this VM-global queue. The
    /// returned claims contain no vPLIC lock and can therefore be completed in
    /// a later short IRQ-disabled section.
    pub(crate) fn take_completed_claim_batch(&self) -> Result<ForwardedCompletionBatch, ()> {
        if !self.is_platform_owner() {
            return Err(());
        }

        let mut sources = [0usize; FORWARDED_COMPLETION_DRAIN_BATCH];
        let source_count = self.vplic.take_completed_forwarded_batch(&mut sources);
        let mut batch = ForwardedCompletionBatch::empty();
        for &source in &sources[..source_count] {
            let encoded = self.notifications.ingress.take_claim(source);
            let Some(claim) = decode_claim(source, encoded) else {
                let mut restored =
                    encoded == 0 || self.notifications.ingress.restore_claim(source, encoded);
                restored &= self.vplic.restore_completed_forwarded_irq(source).is_ok();
                restored &= self.restore_completed_claims(&batch, 0);
                restored &= restore_all(
                    sources[batch.len + 1..source_count].iter().copied(),
                    |unprocessed| {
                        self.vplic
                            .restore_completed_forwarded_irq(unprocessed)
                            .is_ok()
                    },
                );
                self.notifications.ingress.record_fault();
                let published = self.notifications.publish_completion_owner(&self.vplic);
                restored &= published;
                if !restored {
                    self.notifications.ingress.record_fault();
                }
                return Err(());
            };
            batch.claims[batch.len] = Some(claim);
            batch.len += 1;
        }
        if !self.notifications.rearm_completion_owner(&self.vplic) {
            self.restore_completed_claim_batch(&batch, 0);
            self.notifications.ingress.record_fault();
            return Err(());
        }
        Ok(batch)
    }

    fn restore_completed_claim(&self, claim: RiscvPhysicalIrqClaim) -> bool {
        let source = claim.source() as usize;
        let encoded = encode_claim(claim);
        let claim_restored = self.notifications.ingress.restore_claim(source, encoded);
        let completion_restored = self.vplic.restore_completed_forwarded_irq(source).is_ok();
        claim_restored && completion_restored
    }

    pub(crate) fn restore_completed_claim_batch(
        &self,
        batch: &ForwardedCompletionBatch,
        first_uncompleted: usize,
    ) -> bool {
        let restored = self.restore_completed_claims(batch, first_uncompleted);
        let published = self.notifications.publish_completion_owner(&self.vplic);
        restored && published
    }

    fn restore_completed_claims(
        &self,
        batch: &ForwardedCompletionBatch,
        first_uncompleted: usize,
    ) -> bool {
        restore_present_suffix(batch.claims(), first_uncompleted, |claim| {
            self.restore_completed_claim(claim)
        })
    }

    /// Merges one bounded ingress batch into the software vPLIC from the owner
    /// thread before the final IRQ-off guest-entry section.
    pub(crate) fn drain_forwarded_ingress(&self) -> Result<(), ()> {
        if !self.is_platform_owner() {
            return Err(());
        }
        let batch = self.notifications.ingress.take_batch();
        let mut sources = [0usize; FORWARDED_IRQ_DRAIN_BATCH];
        let mut source_count = 0;
        let mut valid = true;
        for entry in batch.entries().iter().copied() {
            let source = entry.source();
            if decode_claim(source, entry.claim())
                .is_none_or(|claim| claim.source() as usize != source)
            {
                valid = false;
            }
            sources[source_count] = source;
            source_count += 1;
        }

        if !valid {
            self.notifications.ingress.record_fault();
            return Err(());
        }

        if source_count != 0 {
            let route_generation = self
                .notifications
                .platform_route_generation()
                .ok_or_else(|| self.notifications.ingress.record_fault())?;
            match self.vplic.set_forwarded_pending_batch_for_generation(
                &sources[..source_count],
                route_generation,
            ) {
                Ok(()) => {
                    for source in &sources[..source_count] {
                        self.notifications.ingress.clear_collision_retry(*source);
                    }
                    publish_changed_contexts(self.vm_id, &self.vplic, &self.notifications);
                }
                Err(riscv_vplic::ForwardedBatchError::Rejected(
                    riscv_vplic::VplicError::ForwardedSourceCollision { .. },
                )) => {
                    if !sources[..source_count]
                        .iter()
                        .all(|source| self.notifications.ingress.begin_collision_retry(*source))
                    {
                        self.notifications.ingress.record_fault();
                        return Err(());
                    }
                    for source in &sources[..source_count] {
                        self.notifications.ingress.requeue(*source);
                    }
                }
                Err(_) => {
                    // Invalid assignment, duplicate ownership, and malformed
                    // controller state cannot heal through rescheduling. Keep
                    // the physical source masked as a quarantine and fail the
                    // vCPU invariant explicitly.
                    self.notifications.ingress.record_fault();
                    return Err(());
                }
            }
        }

        if self.notifications.ingress.rearm_after_drain()
            && !self.notifications.publish_owner(self.context_id)
        {
            self.notifications.ingress.record_fault();
            if self.notifications.ingress.retry_after_failed_wake()
                && !self.notifications.publish_owner(self.context_id)
            {
                self.notifications.ingress.record_fault();
                return Err(());
            }
        }
        Ok(())
    }

    pub(crate) fn finish_completed_claim_batch(
        &self,
        batch: &ForwardedCompletionBatch,
    ) -> Result<(), ()> {
        let route_generation = self
            .notifications
            .platform_route_generation()
            .ok_or_else(|| self.notifications.ingress.record_fault())?;
        let mut sources = [0usize; FORWARDED_COMPLETION_DRAIN_BATCH];
        for (index, claim) in batch.claims().iter().enumerate() {
            let Some(claim) = claim else {
                self.notifications.ingress.record_fault();
                return Err(());
            };
            sources[index] = claim.source() as usize;
        }
        self.vplic
            .finish_forwarded_route_batch(route_generation, &sources[..batch.claims().len()])
            .map_err(|_| self.notifications.ingress.record_fault())
    }

    /// Publishes context-line changes and guest completions after one guest
    /// MMIO write returned to normal task context.
    pub(crate) fn publish_guest_state_changes(&self) -> Result<(), ()> {
        publish_changed_contexts(self.vm_id, &self.vplic, &self.notifications);
        if self.notifications.publish_completion_owner(&self.vplic) {
            Ok(())
        } else {
            self.notifications.ingress.record_fault();
            Err(())
        }
    }
}

/// Masks, drains, and releases the generation-scoped PLIC route for one VM.
#[cfg(any(feature = "fs", feature = "host-fs"))]
pub(crate) fn revoke_guest_irq_routes(vm_id: VMId) -> AxVmResult {
    let Some((route_key, _)) = current_route_identity(&PLATFORM_VPLIC_ROUTE_CONTROL) else {
        return Ok(());
    };
    if route_key.vm_id != vm_id {
        return Ok(());
    }
    let revocation = revoke_active_route(&PLATFORM_VPLIC_ROUTE_CONTROL, route_key)
        .map_err(map_route_reservation_error)?;
    let RouteRevocation::Reserved(revocation_permit) = revocation else {
        return Ok(());
    };
    let generation = revocation_permit.generation();
    PLATFORM_VPLIC_ROUTE.begin_revocation(generation);

    let snapshot = {
        let publisher = PLATFORM_VPLIC_ROUTE
            .acquire_control(generation)
            .ok_or_else(|| {
                AxVmError::resource_unavailable(
                    "RISC-V platform IRQ route revocation",
                    "the published route does not match its transaction generation",
                )
            })?;
        assert_eq!(
            PlatformVplicRouteKey::from_route(publisher.route),
            route_key
        );
        PlatformRouteRevocationSnapshot::from_publisher(&publisher)
    };
    if !snapshot
        .binding
        .notifications
        .begin_platform_revocation(snapshot.binding.context_id, generation)
    {
        return Err(AxVmError::resource_unavailable(
            "RISC-V platform IRQ route revocation",
            "the fixed vPLIC owner or generation changed before quarantine",
        ));
    }

    if !PLATFORM_VPLIC_ROUTE.platform_released(generation) {
        let route = ax_plat::irq::riscv64_hv::RiscvPlicGuestRouteV1::new(
            snapshot.target_cpu,
            snapshot.sources(),
        )
        .map_err(|error| {
            AxVmError::interrupt(
                "encode RISC-V PLIC route revocation",
                format_args!("{error:?}"),
            )
        })?;
        let platform_revocation = ax_plat::irq::riscv64_hv::begin_guest_irq_route_revocation(route)
            .map_err(|error| {
                AxVmError::interrupt(
                    "begin RISC-V PLIC route revocation",
                    format_args!("{error:?}"),
                )
            })?;
        loop {
            match ax_plat::irq::riscv64_hv::poll_guest_irq_route_revocation(platform_revocation)
                .map_err(|error| {
                    AxVmError::interrupt(
                        "poll RISC-V PLIC route revocation",
                        format_args!("{error:?}"),
                    )
                })? {
                ax_plat::irq::riscv64_hv::RiscvPlicRouteRevocationProgress::Pending => {
                    crate::host::task::yield_now();
                }
                ax_plat::irq::riscv64_hv::RiscvPlicRouteRevocationProgress::Released => {
                    PLATFORM_VPLIC_ROUTE.mark_platform_released(generation);
                    break;
                }
            }
        }
    }

    while !PLATFORM_VPLIC_ROUTE.publishers_drained(generation) {
        crate::host::task::yield_now();
    }

    let mut sources = [0usize; PLATFORM_VPLIC_ROUTE_MAX_SOURCES];
    for (index, source) in snapshot.sources().iter().copied().enumerate() {
        sources[index] = source as usize;
    }
    snapshot
        .binding
        .vplic
        .revoke_forwarded_route_batch(generation, &sources[..snapshot.source_count])
        .map_err(|error| {
            AxVmError::interrupt(
                "revoke RISC-V vPLIC forwarded state",
                format_args!("{error}"),
            )
        })?;
    for &source in &sources[..snapshot.source_count] {
        snapshot
            .binding
            .notifications
            .ingress
            .discard_for_revocation(source);
    }
    if !snapshot
        .binding
        .notifications
        .finish_platform_revocation(snapshot.binding.context_id, generation)
    {
        return Err(AxVmError::resource_unavailable(
            "RISC-V platform IRQ route revocation",
            "the vPLIC owner could not finish the quarantined generation",
        ));
    }
    PLATFORM_VPLIC_ROUTE.finish_revocation(generation);
    revocation_permit.finish();
    Ok(())
}

/// Publishes one platform-owned claim into the fixed vPLIC owner ingress.
///
/// # Safety
///
/// This function is called directly from hard-IRQ context. The monitor-wide
/// route, ingress allocation, and wake handle must remain valid until shutdown.
/// The body must remain allocation-free, lock-free, non-blocking, and
/// non-unwinding.
pub(crate) unsafe extern "C" fn forward_unbound_physical_irq(source: u32, generation: u64) -> bool {
    let route = match PLATFORM_VPLIC_ROUTE.acquire_irq() {
        PlatformRouteAcquire::Vacant => return false,
        PlatformRouteAcquire::Quarantined => return true,
        PlatformRouteAcquire::Active(route) => route,
    };
    let Some(claim) = RiscvPhysicalIrqClaim::try_new(source, generation) else {
        route.route.binding.notifications.ingress.record_fault();
        return true;
    };
    if !route.route.contains_source(source) {
        route.route.binding.notifications.ingress.record_fault();
        return true;
    }
    route.route.binding.forward_physical_irq(claim)
}

fn encode_claim(claim: RiscvPhysicalIrqClaim) -> u64 {
    claim.generation()
}

fn decode_claim(source: usize, encoded: u64) -> Option<RiscvPhysicalIrqClaim> {
    RiscvPhysicalIrqClaim::try_new(source as u32, encoded)
}

struct VplicNotifications {
    context_wakes: Box<[Once<ThreadWakeHandle>]>,
    ingress: ForwardedIrqIngress,
    platform_owner_context: FixedOwnerContext,
    platform_route_generation: AtomicU64,
    accepting_forwarded_irqs: AtomicBool,
    completion_doorbell: OwnerDoorbell,
}

impl VplicNotifications {
    fn new(contexts_num: usize) -> Self {
        Self {
            context_wakes: (0..contexts_num)
                .map(|_| Once::new())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            ingress: ForwardedIrqIngress::new(PLIC_NUM_SOURCES),
            platform_owner_context: FixedOwnerContext::new(),
            platform_route_generation: AtomicU64::new(0),
            accepting_forwarded_irqs: AtomicBool::new(false),
            completion_doorbell: OwnerDoorbell::new(),
        }
    }

    fn install(&self, context_id: usize, wake: ThreadWakeHandle) {
        let _installed = self.context_wakes[context_id].call_once(|| wake);
    }

    fn has_owner(&self, context_id: usize) -> bool {
        self.context_wakes
            .get(context_id)
            .is_some_and(|wake| wake.get().is_some())
    }

    fn install_platform_owner(&self, context_id: usize) -> bool {
        self.platform_owner_context.install(context_id)
    }

    fn activate_platform_owner(&self, context_id: usize, generation: u64) -> bool {
        if !self.platform_owner_context.is_owner(context_id) || generation == 0 {
            return false;
        }
        match self.platform_route_generation.compare_exchange(
            0,
            generation,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(installed) if installed == generation => {}
            Err(_) => return false,
        }
        self.accepting_forwarded_irqs.store(true, Ordering::Release);
        true
    }

    fn begin_platform_revocation(&self, context_id: usize, generation: u64) -> bool {
        if self.platform_route_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let accepting = self.accepting_forwarded_irqs.load(Ordering::Acquire);
        let owner = self.platform_owner_context.get();
        // `None + !accepting` is the retryable midpoint after owner release
        // and before generation retirement. No new route can install while
        // the monitor-wide transaction remains Revoking.
        if owner != Some(context_id) && (accepting || owner.is_some()) {
            return false;
        }
        self.accepting_forwarded_irqs
            .store(false, Ordering::Release);
        true
    }

    fn finish_platform_revocation(&self, context_id: usize, generation: u64) -> bool {
        if self.accepting_forwarded_irqs.load(Ordering::Acquire) {
            return false;
        }
        let installed_generation = self.platform_route_generation.load(Ordering::Acquire);
        if installed_generation != generation && installed_generation != 0 {
            return false;
        }
        let owner = self.platform_owner_context.get();
        if owner != Some(context_id) && owner.is_some() {
            return false;
        }
        self.ingress.finish_revocation();
        self.completion_doorbell.clear();
        // Clear the reusable owner before retiring the generation. Both steps
        // are idempotent so a failed-closed caller can retry either midpoint.
        if !self.platform_owner_context.clear(context_id) {
            return false;
        }
        match self.platform_route_generation.compare_exchange(
            generation,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(installed) => installed == 0,
        }
    }

    fn accepts_forwarded_irqs(&self) -> bool {
        self.accepting_forwarded_irqs.load(Ordering::Acquire)
    }

    fn platform_route_generation(&self) -> Option<u64> {
        let generation = self.platform_route_generation.load(Ordering::Acquire);
        (generation != 0).then_some(generation)
    }

    fn is_platform_owner(&self, context_id: usize) -> bool {
        self.platform_owner_context.is_owner(context_id)
    }

    fn platform_owner_context(&self) -> Option<usize> {
        self.platform_owner_context.get()
    }

    fn publish(&self, context_id: usize) {
        if let Some(wake) = self.context_wakes[context_id].get() {
            let _result = wake.wake();
        }
    }

    fn publish_owner(&self, context_id: usize) -> bool {
        self.context_wakes
            .get(context_id)
            .and_then(Once::get)
            .is_some_and(|wake| {
                matches!(
                    wake.wake(),
                    WakeResult::Notified | WakeResult::AlreadyPending
                )
            })
    }

    fn wake_platform_owner(&self) -> bool {
        self.platform_owner_context()
            .is_some_and(|context_id| self.publish_owner(context_id))
    }

    fn publish_completion_owner(&self, vplic: &VPlicGlobal) -> bool {
        self.completion_doorbell.publish_if(
            || vplic.has_completed_forwarded_irq(),
            || self.wake_platform_owner(),
        )
    }

    fn rearm_completion_owner(&self, vplic: &VPlicGlobal) -> bool {
        self.completion_doorbell.rearm_after_drain(
            || vplic.has_completed_forwarded_irq(),
            || self.wake_platform_owner(),
        )
    }
}

struct RiscvPlicIrqSink {
    vm_id: VMId,
    vplic: Arc<VPlicGlobal>,
    notifications: Arc<VplicNotifications>,
}

impl RiscvPlicIrqSink {
    fn update_line(&self, line: IrqLineId, asserted: bool) -> IrqResult {
        update_vplic_line(self.vm_id, &self.vplic, &self.notifications, line, asserted)
    }
}

fn map_route_reservation_error(error: RouteReservationError) -> AxVmError {
    match error {
        RouteReservationError::Conflicting => AxVmError::resource_conflict(
            "RISC-V platform IRQ route",
            "another VM, vCPU, host CPU, or source set owns the passthrough route",
        ),
        RouteReservationError::Preparing
        | RouteReservationError::Published
        | RouteReservationError::Activating
        | RouteReservationError::Revoking => AxVmError::resource_unavailable(
            "RISC-V platform IRQ route",
            format_args!("the matching route transaction is currently {error:?}"),
        ),
        RouteReservationError::Vacant => AxVmError::resource_unavailable(
            "RISC-V platform IRQ route",
            "no prepared route exists for activation",
        ),
    }
}

fn publish_changed_contexts(vm_id: VMId, vplic: &VPlicGlobal, notifications: &VplicNotifications) {
    for context_id in (1..vplic.contexts_num).step_by(2) {
        let changed = match vplic.take_context_notification(context_id) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(error) => {
                warn!("VM[{vm_id}] cannot inspect vPLIC context {context_id}: {error}");
                false
            }
        };
        if !changed {
            continue;
        }

        // Both the VM-owned device sink and deferred physical-IRQ forwarding
        // may run while broader AxVM state is unavailable. A stable scheduler
        // wake handle publishes directly without registry or resource reentry.
        notifications.publish(context_id);
    }
}

fn update_vplic_line(
    vm_id: VMId,
    vplic: &VPlicGlobal,
    notifications: &VplicNotifications,
    line: IrqLineId,
    asserted: bool,
) -> IrqResult {
    let result = if asserted {
        vplic.set_pending(line.0)
    } else {
        vplic.clear_pending(line.0)
    };
    result.map_err(|error| IrqError::Backend {
        line,
        operation: "set vPLIC line level",
        detail: alloc::format!("{error}"),
    })?;
    publish_changed_contexts(vm_id, vplic, notifications);
    Ok(())
}

impl IrqSink for RiscvPlicIrqSink {
    fn set_level(&self, line: IrqLineId, asserted: bool) -> IrqResult {
        self.update_line(line, asserted)
    }

    fn pulse(&self, line: IrqLineId) -> IrqResult {
        self.update_line(line, true)
    }
}

struct RiscvPlicFactory {
    base_gpa: usize,
    length: usize,
    contexts_num: usize,
    vplic: Arc<VPlicGlobal>,
}

impl DeviceFactory for RiscvPlicFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::PPPTGlobal
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        if config.base_gpa != self.base_gpa
            || config.length != self.length
            || config.cfg_list.as_slice() != [self.contexts_num]
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build virtual PLIC",
                detail: alloc::format!(
                    "factory configuration does not match device '{}'",
                    config.name
                ),
            });
        }
        Ok(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(self.vplic.clone())).into())
    }
}

fn validate_vplic_config(config: &EmulatedDeviceConfig) -> AxVmResult<usize> {
    let [contexts_num] = config.cfg_list.as_slice() else {
        return ax_err!(
            InvalidInput,
            format_args!(
                "virtual PLIC device '{}' requires exactly one context-count argument",
                config.name
            )
        );
    };
    let context_end = contexts_num
        .checked_mul(PLIC_CONTEXT_STRIDE)
        .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CTRL_OFFSET))
        .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
        .and_then(|offset| config.base_gpa.checked_add(offset))
        .ok_or_else(|| ax_err_type!(InvalidInput, "virtual PLIC context range overflow"))?;
    let region_end = config
        .base_gpa
        .checked_add(config.length)
        .ok_or_else(|| ax_err_type!(InvalidInput, "virtual PLIC region range overflow"))?;
    if region_end <= context_end {
        return ax_err!(
            InvalidInput,
            format_args!(
                "virtual PLIC device '{}' range [{:#x}, {:#x}) does not cover {} contexts",
                config.name, config.base_gpa, region_end, contexts_num
            )
        );
    }
    Ok(*contexts_num)
}

pub(crate) fn configure(
    vm_id: VMId,
    factories: &mut DeviceFactoryRegistry,
    mode: VMInterruptMode,
    configs: &[EmulatedDeviceConfig],
) -> AxVmResult<RiscvInterruptResources> {
    let mut vplic_configs = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::PPPTGlobal);
    let Some(config) = vplic_configs.next() else {
        return Ok(RiscvInterruptResources {
            interrupt_fabric: InterruptFabric::new(mode),
            vplic: None,
        });
    };
    if vplic_configs.next().is_some() {
        return ax_err!(
            AlreadyExists,
            "a VM can register only one virtual PLIC global controller"
        );
    }

    let contexts_num = validate_vplic_config(config)?;
    let vplic = Arc::new(
        VPlicGlobal::new(config.base_gpa.into(), Some(config.length), contexts_num)
            .map_err(AxVmError::invalid_config)?,
    );
    let notifications = Arc::new(VplicNotifications::new(contexts_num));
    factories.register(Arc::new(RiscvPlicFactory {
        base_gpa: config.base_gpa,
        length: config.length,
        contexts_num,
        vplic: vplic.clone(),
    }))?;

    let interrupt_fabric = InterruptFabric::with_sink(
        mode,
        Arc::new(RiscvPlicIrqSink {
            vm_id,
            vplic: vplic.clone(),
            notifications: notifications.clone(),
        }),
    )?;
    Ok(RiscvInterruptResources {
        interrupt_fabric,
        vplic: Some(VplicResources {
            vm_id,
            vplic,
            notifications,
        }),
    })
}

pub(crate) fn bind_vcpu(
    vplic: Option<&VplicResources>,
    vcpu_id: usize,
) -> AxVmResult<Option<VplicVcpuBinding>> {
    let Some(vplic) = vplic else {
        return Ok(None);
    };
    let context_id = vcpu_id
        .checked_mul(2)
        .and_then(|id| id.checked_add(1))
        .ok_or_else(|| ax_err_type!(InvalidInput, "RISC-V vPLIC context ID overflow"))?;
    if context_id >= vplic.vplic.contexts_num {
        return ax_err!(
            InvalidInput,
            format_args!(
                "RISC-V vCPU {vcpu_id} requires supervisor context {context_id}, but vPLIC has {} \
                 contexts",
                vplic.vplic.contexts_num
            )
        );
    }
    Ok(Some(VplicVcpuBinding {
        vm_id: vplic.vm_id,
        vplic: vplic.vplic.clone(),
        notifications: vplic.notifications.clone(),
        context_id,
    }))
}
