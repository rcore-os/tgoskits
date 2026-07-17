//! Runtime ownership and activation of portable block controllers.

mod device;
mod error;
mod irq_routes;
mod owner;
mod recovery;
mod recovery_irq;
mod registry;
mod source;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_driver::block::RdifBlockDevice;
use ax_hal::irq::{IrqDrainToken, IrqDrainWake};
use ax_kspin::SpinNoPreempt;
pub use device::BlockDeviceView;
pub(in crate::block) use device::{InlineQueue, RuntimeBlockDevice, RuntimeQueue};
pub use error::{BlockControllerError, BlockHandoffError};
use irq_routes::HandoffIrqOwner;
pub(in crate::block) use owner::ControllerOwnerLink;
use owner::controller_irq_drain_wake;
use rdif_block::{
    DmaQuiesced, InitError, InitInput, InitPoll, LifecycleEndpoint, RecoveryCause,
    validate_lifecycle_activation, validate_lifecycle_identity,
};
use recovery::{RecoveryStep, controller_recovery_timer_entry, controller_recovery_work_entry};
use source::RuntimeIrqSource;
pub(in crate::block) use registry::runtime_handoff_controllers;
pub use registry::{activate_discovered_controllers, block_io_stats};
use registry::{
    capture_host_physical_ranges, capture_pci_endpoint, create_runtime_devices,
    materialize_logical_devices, register_irq_routes_disabled,
    rollback_unpublished_runtime_devices, shutdown_logical_device_iter, validate_runtime_devices,
};

use super::{
    activation::drive_controller_initialization,
    handoff::{BlockControllerIdentity, HostPciEndpoint, HostPhysicalRange},
    statistics::BlockIoStats,
};
use crate::{
    irq::{DetachedRegistration, Registration},
    task::WaitQueue,
    workqueue::{DelayedWork, WorkItem, WorkPriority, WorkQueue},
};

const MAX_HARDWARE_QUEUES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ControllerPhase {
    Running    = 0,
    Quiescing  = 1,
    Recovering = 2,
    GuestOwned = 3,
    Offline    = 4,
}

impl ControllerPhase {
    fn decode(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Quiescing,
            2 => Self::Recovering,
            3 => Self::GuestOwned,
            _ => Self::Offline,
        }
    }
}

/// Shutdown-lifetime owner of a complete controller bundle and its IRQ leases.
///
/// The retained [`RdifBlockDevice`] is intentional: PCI MSI-X/INTx binding
/// leases are part of that object and must outlive every queue, IRQ callback,
/// and DMA request. Filesystem-facing clones hold this controller through an
/// [`Arc`], while the runtime registry retains one reference until shutdown.
pub struct BlockController {
    name: String,
    host_physical_ranges: Box<[HostPhysicalRange]>,
    pci_endpoint: Option<HostPciEndpoint>,
    pub(super) devices: Box<[RuntimeBlockDevice]>,
    registrations: SpinNoPreempt<Option<Box<[Registration]>>>,
    irq_sources: Box<[&'static RuntimeIrqSource]>,
    detached_registrations: SpinNoPreempt<Option<Box<[DetachedRegistration]>>>,
    device: SpinNoPreempt<RdifBlockDevice>,
    phase: AtomicU8,
    handoff_reserved: AtomicBool,
    active_operations: AtomicUsize,
    operation_wait: WaitQueue,
    recovery_domain: Pin<&'static WorkQueue>,
    recovery_work: WorkItem,
    recovery_timer: DelayedWork,
    recovery_step: AtomicU8,
    recovery_epoch: AtomicU64,
    irq_recovery_queues: AtomicU64,
    recovery_cause: SpinNoPreempt<Option<RecoveryCause>>,
    lifecycle_cookie: usize,
    recovery_wait_sources: AtomicU64,
    recovery_pending_sources: AtomicU64,
    recovery_polling_irqs: AtomicBool,
    recovery_irqs_enabled: AtomicBool,
    recovery_failed: AtomicBool,
    recovery_irq_drains: SpinNoPreempt<[Option<IrqDrainToken>; MAX_HARDWARE_QUEUES]>,
    irq_drain_wake: IrqDrainWake,
    owner_link: &'static ControllerOwnerLink,
}

pub(super) struct ControllerOperation<'controller> {
    controller: &'controller BlockController,
}

pub(super) struct ControllerHandoffReservation {
    controller: Arc<BlockController>,
    identity: BlockControllerIdentity,
    armed: bool,
}

pub(super) struct GuestOwnedControllerLease {
    controller: Arc<BlockController>,
    identity: BlockControllerIdentity,
}

pub(super) struct QuarantinedControllerLease {
    controller: Arc<BlockController>,
    identity: BlockControllerIdentity,
}

pub(super) struct ControllerCommitFailure {
    pub(super) error: BlockHandoffError,
    pub(super) quarantined: QuarantinedControllerLease,
}

pub(super) struct ControllerReturnFailure {
    pub(super) error: BlockHandoffError,
    pub(super) quarantined: QuarantinedControllerLease,
}

impl Drop for ControllerOperation<'_> {
    fn drop(&mut self) {
        let previous = self
            .controller
            .active_operations
            .fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous != 0,
            "block controller operation count underflowed"
        );
        if previous == 1 {
            self.controller.operation_wait.notify_all();
        }
    }
}

impl ControllerHandoffReservation {
    pub(super) const fn identity(&self) -> BlockControllerIdentity {
        self.identity
    }

    pub(super) fn commit(mut self) -> Result<GuestOwnedControllerLease, ControllerCommitFailure> {
        let result = self.controller.commit_handoff();
        self.controller
            .handoff_reserved
            .store(false, Ordering::Release);
        self.armed = false;
        match result {
            Ok(()) => Ok(GuestOwnedControllerLease {
                controller: Arc::clone(&self.controller),
                identity: self.identity,
            }),
            Err(error) => {
                self.controller.mark_offline();
                Err(ControllerCommitFailure {
                    error,
                    quarantined: QuarantinedControllerLease {
                        controller: Arc::clone(&self.controller),
                        identity: self.identity,
                    },
                })
            }
        }
    }
}

impl Drop for ControllerHandoffReservation {
    fn drop(&mut self) {
        if self.armed {
            self.controller
                .handoff_reserved
                .store(false, Ordering::Release);
        }
    }
}

impl GuestOwnedControllerLease {
    pub(super) const fn identity(&self) -> BlockControllerIdentity {
        self.identity
    }

    pub(super) fn quarantine(self) -> QuarantinedControllerLease {
        self.controller.mark_offline();
        QuarantinedControllerLease {
            controller: self.controller,
            identity: self.identity,
        }
    }

    pub(super) fn return_from_guest(
        self,
    ) -> Result<BlockControllerIdentity, ControllerReturnFailure> {
        match self.controller.return_from_guest() {
            Ok(()) => Ok(self.identity),
            Err(error) => {
                self.controller.mark_offline();
                Err(ControllerReturnFailure {
                    error,
                    quarantined: QuarantinedControllerLease {
                        controller: self.controller,
                        identity: self.identity,
                    },
                })
            }
        }
    }
}

impl QuarantinedControllerLease {
    pub(super) const fn identity(&self) -> BlockControllerIdentity {
        self.identity
    }

    pub(super) fn controller_name(&self) -> &str {
        self.controller.name()
    }
}

impl BlockController {
    /// Activates one discovered controller after shared workers are online.
    ///
    /// Every callback target is pinned and every queue/source contract is
    /// validated before device-side or OS-side interrupt delivery is enabled.
    ///
    /// # Errors
    ///
    /// Returns a typed error and leaves the device unpublished if queue, IRQ,
    /// or driver activation cannot satisfy the IRQ-only contract.
    pub fn activate(device: RdifBlockDevice) -> Result<Arc<Self>, BlockControllerError> {
        let mut device = drive_controller_initialization(device)?;
        let name = String::from(device.name());
        let host_physical_ranges = capture_host_physical_ranges(&device)?;
        let pci_endpoint = capture_pci_endpoint(&device);
        let owner_link = Box::leak(Box::new(ControllerOwnerLink::new()));
        let logical_devices = materialize_logical_devices(&mut device)?;
        let queue_kinds = logical_devices
            .iter()
            .flat_map(|device| device.queues())
            .map(|queue| queue.info().kind)
            .collect::<Vec<_>>();
        let (lifecycle_kind, lifecycle_cookie) = match device.bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => (rdif_block::LifecycleKind::Inline, 0),
            LifecycleEndpoint::Interrupt(lifecycle) => (
                rdif_block::LifecycleKind::Interrupt,
                lifecycle.controller_cookie(),
            ),
        };
        if let Err(error) = validate_lifecycle_activation(&queue_kinds, lifecycle_kind) {
            shutdown_logical_device_iter(logical_devices.into_iter());
            return Err(error.into());
        }
        if let Err(error) = validate_lifecycle_identity(lifecycle_kind, lifecycle_cookie) {
            shutdown_logical_device_iter(logical_devices.into_iter());
            return Err(error.into());
        }
        let devices = create_runtime_devices(logical_devices, owner_link, lifecycle_cookie)?;
        let declared_sources = device.bundle().irq_sources();
        let (registrations, irq_sources, bound_sources) = match register_irq_routes_disabled(
            &name,
            &mut device,
            &devices,
            &declared_sources,
            owner_link,
        ) {
            Ok(routes) => routes,
            Err(error) => {
                rollback_unpublished_runtime_devices(&devices);
                return Err(error);
            }
        };
        if let Err(error) = validate_runtime_devices(&devices, &declared_sources, bound_sources) {
            drop(registrations);
            rollback_unpublished_runtime_devices(&devices);
            return Err(error);
        }

        let recovery_cpu = devices
            .iter()
            .flat_map(|device| device.queues.iter())
            .find_map(RuntimeQueue::interrupt_queue)
            .map_or(0, |queue| queue.affinity_cpu());
        let recovery_domain = Box::leak(Box::new(WorkQueue::new(recovery_cpu, WorkPriority::High)));
        let recovery_domain = unsafe {
            // SAFETY: logical controller domains have shutdown lifetime and
            // are never moved after their intrusive work item is published.
            Pin::new_unchecked(&*recovery_domain)
        };
        let link_address = ptr::from_ref(owner_link).expose_provenance();
        let controller = Arc::new(Self {
            name,
            host_physical_ranges,
            pci_endpoint,
            devices: devices.into_boxed_slice(),
            registrations: SpinNoPreempt::new(Some(registrations.into_boxed_slice())),
            irq_sources: irq_sources.into_boxed_slice(),
            detached_registrations: SpinNoPreempt::new(None),
            device: SpinNoPreempt::new(device),
            phase: AtomicU8::new(ControllerPhase::Running as u8),
            handoff_reserved: AtomicBool::new(false),
            active_operations: AtomicUsize::new(0),
            operation_wait: WaitQueue::new(),
            recovery_domain,
            recovery_work: WorkItem::new(controller_recovery_work_entry, link_address),
            recovery_timer: DelayedWork::new(controller_recovery_timer_entry, link_address),
            recovery_step: AtomicU8::new(RecoveryStep::Idle as u8),
            recovery_epoch: AtomicU64::new(rdif_block::ControllerEpoch::INITIAL.get()),
            irq_recovery_queues: AtomicU64::new(0),
            recovery_cause: SpinNoPreempt::new(None),
            lifecycle_cookie,
            recovery_wait_sources: AtomicU64::new(0),
            recovery_pending_sources: AtomicU64::new(0),
            recovery_polling_irqs: AtomicBool::new(false),
            recovery_irqs_enabled: AtomicBool::new(false),
            recovery_failed: AtomicBool::new(false),
            recovery_irq_drains: SpinNoPreempt::new([const { None }; MAX_HARDWARE_QUEUES]),
            irq_drain_wake: unsafe {
                // SAFETY: owner_link is leaked for shutdown lifetime before
                // this target is constructed. The callback only Acquire-loads
                // that stable link and queues the controller's fixed recovery
                // item, so it is allocation-free and hard-IRQ-safe.
                IrqDrainWake::new(link_address, controller_irq_drain_wake)
            },
            owner_link,
        });
        owner_link.publish(&controller);

        let has_irq_routes = controller
            .registrations
            .lock()
            .as_ref()
            .is_some_and(|registrations| !registrations.is_empty());
        if has_irq_routes {
            let action_error = {
                let registrations = controller.registrations.lock();
                registrations.as_ref().and_then(|registrations| {
                    registrations
                        .iter()
                        .find_map(|registration| registration.enable().err())
                })
            };
            if let Some(error) = action_error {
                controller.abort_failed_activation();
                return Err(error.into());
            }
            if let Err(error) = controller.device.lock().enable_irq() {
                controller.abort_failed_activation();
                return Err(error.into());
            }
            controller
                .recovery_irqs_enabled
                .store(true, Ordering::Release);
        }

        Ok(controller)
    }

    /// Returns the stable driver diagnostic name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns aggregate successful I/O counters for all logical devices.
    pub fn io_stats(&self) -> BlockIoStats {
        self.devices
            .iter()
            .fold(BlockIoStats::default(), |total, device| {
                total.saturating_add(device.statistics.snapshot())
            })
    }

    pub(super) fn begin_operation(&self) -> Option<ControllerOperation<'_>> {
        if self.phase() != ControllerPhase::Running {
            return None;
        }
        self.active_operations.fetch_add(1, Ordering::AcqRel);
        if self.phase() != ControllerPhase::Running {
            let previous = self.active_operations.fetch_sub(1, Ordering::AcqRel);
            assert!(
                previous != 0,
                "block controller operation count underflowed"
            );
            if previous == 1 {
                self.operation_wait.notify_all();
            }
            return None;
        }
        Some(ControllerOperation { controller: self })
    }

    fn phase(&self) -> ControllerPhase {
        ControllerPhase::decode(self.phase.load(Ordering::Acquire))
    }

    pub(super) fn has_interrupt_queues(&self) -> bool {
        self.runtime_queues()
            .any(|queue| matches!(queue, RuntimeQueue::Interrupt(_)))
    }

    pub(super) fn host_physical_ranges(&self) -> &[HostPhysicalRange] {
        &self.host_physical_ranges
    }

    pub(super) const fn pci_endpoint(&self) -> Option<HostPciEndpoint> {
        self.pci_endpoint
    }

    pub(super) fn reserve_handoff(
        self: &Arc<Self>,
        identity: BlockControllerIdentity,
    ) -> Result<ControllerHandoffReservation, BlockHandoffError> {
        if self.phase() != ControllerPhase::Running
            || self
                .handoff_reserved
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        if self.phase() != ControllerPhase::Running
            || !self.has_interrupt_queues()
            || self.registrations.lock().is_none()
            || self.detached_registrations.lock().is_some()
        {
            self.handoff_reserved.store(false, Ordering::Release);
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        Ok(ControllerHandoffReservation {
            controller: Arc::clone(self),
            identity,
            armed: true,
        })
    }

    fn commit_handoff(self: &Arc<Self>) -> Result<(), BlockHandoffError> {
        if !self.handoff_reserved.load(Ordering::Acquire) {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        self.phase
            .compare_exchange(
                ControllerPhase::Running as u8,
                ControllerPhase::Quiescing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| BlockHandoffError::InvalidState(self.name.clone()))?;

        self.operation_wait
            .try_wait_until(|| self.active_operations.load(Ordering::Acquire) == 0)?;
        if self.phase() != ControllerPhase::Quiescing {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }

        let mut quiesced = Vec::new();
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                match queue.quiesce_and_drain() {
                    Ok(permit) => quiesced.push(permit),
                    Err(error) => {
                        self.mark_offline();
                        return Err(error.into());
                    }
                }
            }
        }

        let mut irq_owner = HandoffIrqOwner::take(self)?;
        if let Err(error) = irq_owner.mask_device() {
            // The retained, still-enabled actions remain the only safe owner
            // of an interrupt source whose device-side mask did not commit.
            self.mark_offline();
            return Err(error.into());
        }
        for registration in irq_owner.actions() {
            if let Err(error) = registration.disable() {
                irq_owner.fail_closed();
                return Err(error.into());
            }
        }
        for registration in irq_owner.actions() {
            if let Err(error) = registration.synchronize() {
                irq_owner.fail_closed();
                return Err(error.into());
            }
        }
        if let Err(error) = irq_owner.detach_actions() {
            irq_owner.fail_closed();
            return Err(error.into());
        }

        let mut service_drained = Vec::with_capacity(quiesced.len());
        for permit in quiesced {
            match permit.drain_service_work() {
                Ok(permit) => service_drained.push(permit),
                Err(error) => {
                    irq_owner.fail_closed();
                    return Err(error.into());
                }
            }
        }

        let proof = if service_drained.is_empty() {
            None
        } else {
            match self.drive_handoff_dma_quiesce() {
                Ok(proof) => Some(proof),
                Err(error) => {
                    irq_owner.fail_closed();
                    return Err(error);
                }
            }
        };

        for permit in service_drained {
            let proof = proof
                .as_ref()
                .expect("an interrupt hctx requires one controller DMA proof");
            if let Err(error) = permit.detach_after_dma_quiesce(proof) {
                irq_owner.fail_closed();
                return Err(error.into());
            }
        }
        if let Some(proof) = proof {
            let transition = match self.device.lock().bundle_mut().lifecycle() {
                LifecycleEndpoint::Inline => Err(InitError::InvalidState),
                LifecycleEndpoint::Interrupt(lifecycle) => lifecycle.enter_guest_owned(proof),
            };
            if let Err(error) = transition {
                irq_owner.fail_closed();
                return Err(error.into());
            }
        }
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue
                && let Err(error) = queue.enter_guest_owned()
            {
                irq_owner.fail_closed();
                return Err(error.into());
            }
        }

        self.recovery_irqs_enabled.store(false, Ordering::Release);
        // Guest ownership becomes observable only after the controller owns
        // every callback token needed for a fail-closed host return.
        irq_owner.publish_detached_actions();
        if self
            .phase
            .compare_exchange(
                ControllerPhase::Quiescing as u8,
                ControllerPhase::GuestOwned as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            self.mark_offline();
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        Ok(())
    }

    fn drive_handoff_dma_quiesce(&self) -> Result<DmaQuiesced, BlockHandoffError> {
        if self.phase() != ControllerPhase::Quiescing {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        let epoch = self.advance_recovery_epoch()?;
        let begin = match self.device.lock().bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => Err(InitError::InvalidState),
            LifecycleEndpoint::Interrupt(lifecycle) => {
                if lifecycle.controller_cookie() != self.lifecycle_cookie {
                    Err(InitError::InvalidState)
                } else {
                    lifecycle.begin_dma_quiesce(epoch, RecoveryCause::Handoff)
                }
            }
        };
        begin?;

        loop {
            let input = InitInput::at(ax_hal::time::monotonic_time_nanos());
            let progress = match self.device.lock().bundle_mut().lifecycle() {
                LifecycleEndpoint::Inline => InitPoll::Failed(InitError::InvalidState),
                LifecycleEndpoint::Interrupt(lifecycle) => lifecycle.poll_dma_quiesce(input),
            };
            match progress {
                InitPoll::Ready(proof) => {
                    self.validate_dma_proof(&proof)?;
                    return Ok(proof);
                }
                InitPoll::Pending(schedule) => {
                    let schedule = schedule.validate()?;
                    if !schedule.irq_sources().is_empty() {
                        return Err(InitError::MissingInterrupt.into());
                    }
                    if schedule.run_again() {
                        let _decision = crate::task::yield_current_cpu()?;
                    } else if let Some(deadline_ns) = schedule.wake_at_ns() {
                        crate::task::sleep_until(Duration::from_nanos(deadline_ns));
                    }
                }
                InitPoll::Failed(error) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::block::statistics::BlockIoCounters;

    #[test]
    fn block_statistics_use_linux_512_byte_sector_units() {
        let counters = BlockIoCounters::new();

        counters.record_success(rdif_block::RequestOp::Read, 4096);
        counters.record_success(rdif_block::RequestOp::Write, 513);

        let snapshot = counters.snapshot();
        assert_eq!(snapshot.reads_completed(), 1);
        assert_eq!(snapshot.sectors_read(), 8);
        assert_eq!(snapshot.writes_completed(), 1);
        assert_eq!(snapshot.sectors_written(), 2);
    }
}
