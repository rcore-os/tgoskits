//! Runtime ownership and activation of portable block controllers.

mod device;
mod error;
mod handoff_owner;
mod irq_routes;
mod owner;
mod prepared;
mod recovery;
mod registry;
mod shutdown_owner;
pub(in crate::block) mod source;
mod topology;

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use ax_driver::block::RdifBlockDevice;
use ax_kspin::{IrqGuard, SpinNoPreempt};
pub use device::BlockDeviceView;
pub(in crate::block) use device::{
    DeviceSoftwareContexts, InlineQueue, RuntimeBlockDevice, RuntimeQueue,
};
pub use error::{BlockControllerError, BlockHandoffError};
pub(super) use handoff_owner::OwnerHandoff;
pub(in crate::block) use owner::ControllerOwnerLink;
pub(in crate::block) use prepared::PreparedBlockController;
use rdif_block::RecoveryCause;
pub(in crate::block) use registry::runtime_handoff_controllers;
pub use registry::{
    activate_discovered_controllers, activate_discovered_controllers_with_config, block_io_stats,
};
pub(in crate::block) use shutdown_owner::{OwnerShutdown, OwnerShutdownProgress};
use source::BlockMaintenanceEvent;
pub(in crate::block) use topology::{IrqLineOwnershipReservation, OwnershipDomainTopology};

use super::{
    HardwareQueueError,
    activation::activate_controller,
    handoff::{BlockControllerIdentity, HostPciEndpoint, HostPhysicalRange},
    statistics::BlockIoStats,
};
use crate::{
    maintenance::{DeviceMaintenanceHandle, MaintenanceThread},
    task::WaitQueue,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(in crate::block) enum OwnerCommand {
    None          = 0,
    Handoff       = 1,
    ReturnHost    = 2,
    Preparing     = 3,
    ReturnWaiting = 4,
}

impl OwnerCommand {
    fn decode(value: u8) -> Self {
        match value {
            1 => Self::Handoff,
            2 => Self::ReturnHost,
            3 => Self::Preparing,
            4 => Self::ReturnWaiting,
            _ => Self::None,
        }
    }
}

pub(in crate::block) const BLOCK_OWNER_CONTROL_CAUSE: crate::maintenance::MaintenanceCauses =
    crate::maintenance::MaintenanceCauses::from_bits(1 << 8);

/// Short-lock storage for the portable controller control endpoint.
///
/// The spin lock protects only the ownership handoff into and out of a linear
/// lease. Portable driver callbacks run after the slot guard has been dropped,
/// on the validated maintenance owner thread.
struct DriverEndpointSlot<T> {
    endpoint: SpinNoPreempt<Option<T>>,
}

impl<T> DriverEndpointSlot<T> {
    fn new(endpoint: T) -> Self {
        Self {
            endpoint: SpinNoPreempt::new(Some(endpoint)),
        }
    }

    fn lease(&self) -> DriverEndpointLease<'_, T> {
        // The portable control endpoint and its hard-IRQ endpoint can share
        // one register block. This local exclusion must begin before moving
        // the endpoint out of its slot so the same-CPU top half cannot observe
        // a half-completed task-side register transaction.
        let irq_guard = IrqGuard::new();
        let endpoint = self
            .endpoint
            .lock()
            .take()
            .expect("portable block driver endpoint lease is already active");
        DriverEndpointLease {
            slot: self,
            endpoint: Some(endpoint),
            irq_guard: Some(irq_guard),
            _not_send: PhantomData,
        }
    }
}

/// Linear owner-thread borrow of one portable controller endpoint.
///
/// This lease is intentionally `!Send`. Its destructor restores the complete
/// endpoint owner even when a portable callback returns an error through `?`.
struct DriverEndpointLease<'slot, T> {
    slot: &'slot DriverEndpointSlot<T>,
    endpoint: Option<T>,
    irq_guard: Option<IrqGuard>,
    _not_send: PhantomData<*mut ()>,
}

impl<T> DriverEndpointLease<'_, T> {
    fn endpoint_mut(&mut self) -> &mut T {
        self.endpoint
            .as_mut()
            .expect("portable block driver endpoint lease lost its owner")
    }
}

impl<T> Drop for DriverEndpointLease<'_, T> {
    fn drop(&mut self) {
        let endpoint = self
            .endpoint
            .take()
            .expect("portable block driver endpoint lease restored twice");
        let mut slot = self.slot.endpoint.lock();
        assert!(
            slot.is_none(),
            "portable block driver endpoint slot was replaced while leased"
        );
        *slot = Some(endpoint);
        drop(slot);
        // Restoring local IRQs before publishing the endpoint back into its
        // stable slot would let the hard handler race endpoint teardown.
        drop(self.irq_guard.take());
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
    device: DriverEndpointSlot<RdifBlockDevice>,
    maintenance: Arc<DeviceMaintenanceHandle<BlockMaintenanceEvent>>,
    maintenance_thread: SpinNoPreempt<Option<MaintenanceThread>>,
    owner_command: AtomicU8,
    owner_command_result: SpinNoPreempt<Option<Result<(), BlockHandoffError>>>,
    owner_command_wait: WaitQueue,
    phase: AtomicU8,
    handoff_reserved: AtomicBool,
    active_operations: AtomicUsize,
    operation_wait: WaitQueue,
    recovery_step: AtomicU8,
    recovery_deadline_ns: AtomicU64,
    recovery_epoch: AtomicU64,
    irq_recovery_queues: AtomicU64,
    recovery_cause: SpinNoPreempt<Option<RecoveryCause>>,
    lifecycle_cookie: usize,
    recovery_wait_sources: AtomicU64,
    recovery_pending_sources: AtomicU64,
    recovery_irqs_enabled: AtomicBool,
    owner_link: Arc<ControllerOwnerLink>,
    ownership_topology: OwnershipDomainTopology,
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
            let _ = self
                .controller
                .maintenance
                .publish_cause(BLOCK_OWNER_CONTROL_CAUSE);
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
    /// Activates one discovered controller on its final maintenance owner.
    ///
    /// Every callback target is pinned and every queue/source contract is
    /// validated before device-side or OS-side interrupt delivery is enabled.
    ///
    /// # Errors
    ///
    /// Returns a typed error and leaves the device unpublished if queue, IRQ,
    /// or driver activation cannot satisfy the IRQ-only contract.
    pub fn activate(device: RdifBlockDevice) -> Result<Arc<Self>, BlockControllerError> {
        Self::activate_with_config(device, crate::block::BlockRuntimeConfig::default())
    }

    /// Activates one discovered controller with explicit OS watchdog policy.
    pub fn activate_with_config(
        device: RdifBlockDevice,
        config: crate::block::BlockRuntimeConfig,
    ) -> Result<Arc<Self>, BlockControllerError> {
        activate_controller(device, config)
    }

    /// Returns the stable driver diagnostic name.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn install_maintenance_thread(&self, thread: MaintenanceThread) {
        let mut slot = self.maintenance_thread.lock();
        assert!(slot.is_none(), "block maintenance thread installed twice");
        *slot = Some(thread);
    }

    pub(super) fn clear_owner_link_after_drain(&self) {
        self.owner_link.clear_after_drain(self);
    }

    pub(super) fn enable_device_irq_on_owner(&self) -> Result<(), BlockControllerError> {
        self.with_driver_endpoint_on_owner(|device| device.enable_irq())?;
        self.recovery_irqs_enabled.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn disable_device_irq_on_owner(&self) -> Result<(), BlockControllerError> {
        self.with_driver_endpoint_on_owner(|device| device.disable_irq())?;
        self.recovery_irqs_enabled.store(false, Ordering::Release);
        Ok(())
    }

    /// Runs one portable control operation on the sole maintenance owner.
    ///
    /// CPU/thread validation and the slot ownership transfer complete before
    /// `operation` is entered. No spin or preemption guard spans driver code.
    pub(super) fn with_driver_endpoint_on_owner<R>(
        &self,
        operation: impl FnOnce(&mut RdifBlockDevice) -> R,
    ) -> R {
        self.assert_driver_endpoint_owner();
        let mut endpoint = self.device.lease();
        operation(endpoint.endpoint_mut())
    }

    fn assert_driver_endpoint_owner(&self) {
        assert!(
            !ax_hal::irq::in_irq_context(),
            "portable block driver control callback entered from hard IRQ"
        );
        let current = crate::task::current_thread_id()
            .expect("portable block driver control callback requires a scheduler thread");
        assert_eq!(
            current,
            self.maintenance.owner_thread(),
            "portable block driver control callback entered from a non-owner thread"
        );
        let owner_cpu = self.ownership_topology.owner_cpu();
        assert_eq!(
            owner_cpu,
            self.maintenance.owner_cpu(),
            "block IRQ ownership and maintenance owner CPU diverged"
        );
        let cpu = ax_hal::percpu::this_cpu_id();
        assert_eq!(
            cpu, owner_cpu,
            "portable block driver control callback entered from a non-owner CPU"
        );
    }

    pub(super) fn record_owner_service_failure(&self, error: &HardwareQueueError) {
        error!(
            "block controller {} maintenance owner detected a queue fault: {error}",
            self.name
        );
        let queue_id = self
            .runtime_queues()
            .filter_map(RuntimeQueue::interrupt_queue)
            .map(|queue| queue.info().id)
            .next()
            .unwrap_or(0);
        self.schedule_recovery(RecoveryCause::QueueFault { queue_id });
    }

    pub(super) fn route_owner_irq(
        &self,
        source_id: usize,
        source_epoch: u64,
        facts: rdif_block::Event,
    ) -> Result<(), HardwareQueueError> {
        match self.phase() {
            ControllerPhase::Recovering => {
                return self
                    .record_recovery_irq(source_id)
                    .then_some(())
                    .ok_or(HardwareQueueError::StaleIrqEvent);
            }
            ControllerPhase::Running | ControllerPhase::Quiescing => {}
            ControllerPhase::GuestOwned | ControllerPhase::Offline => {
                return Err(HardwareQueueError::Offline);
            }
        }
        let epoch = rdif_block::IrqEventEpoch::new(source_epoch)
            .ok_or(HardwareQueueError::StaleIrqEvent)?;
        let event = rdif_block::AcknowledgedEvent::new(source_id, epoch, facts);
        let controller_epoch = self.recovery_epoch.load(Ordering::Acquire);
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue
                && let Err(error) = queue.record_owner_irq_event(controller_epoch, event)
            {
                self.schedule_recovery(RecoveryCause::QueueFault {
                    queue_id: queue.info().id,
                });
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn service_owner_queues(&self) -> Result<bool, HardwareQueueError> {
        if !self.normal_irq_service_active() {
            return Ok(false);
        }
        let mut more = false;
        for queue in self.runtime_queues() {
            if let Some(queue) = queue.interrupt_queue() {
                more |= matches!(
                    queue.service_bounded()?,
                    super::hctx::OwnerServiceProgress::More
                );
            }
        }
        Ok(more)
    }

    pub(super) fn next_owner_deadline_ns(&self) -> Option<u64> {
        self.runtime_queues()
            .filter_map(RuntimeQueue::interrupt_queue)
            .filter_map(|queue| queue.next_deadline_ns())
            .chain({
                let deadline = self.recovery_deadline_ns.load(Ordering::Acquire);
                (deadline != 0).then_some(deadline)
            })
            .min()
    }

    pub(super) fn raise_due_watchdogs(&self, now_ns: u64) {
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue
                && queue
                    .next_deadline_ns()
                    .is_some_and(|deadline| deadline <= now_ns)
            {
                queue.raise_owner_watchdog();
            }
        }
    }

    fn request_owner_command(&self, command: OwnerCommand) -> Result<(), BlockHandoffError> {
        if command == OwnerCommand::None
            || self
                .owner_command
                .compare_exchange(
                    OwnerCommand::None as u8,
                    OwnerCommand::Preparing as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        *self.owner_command_result.lock() = None;
        self.owner_command.store(command as u8, Ordering::Release);
        if self
            .maintenance
            .publish_cause(BLOCK_OWNER_CONTROL_CAUSE)
            .is_err()
        {
            self.owner_command
                .store(OwnerCommand::None as u8, Ordering::Release);
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        self.owner_command_wait
            .try_wait_until(|| self.owner_command_result.lock().is_some())?;
        self.owner_command_result
            .lock()
            .take()
            .expect("owner command result was observed before take")
    }

    pub(super) fn current_owner_command(&self) -> OwnerCommand {
        OwnerCommand::decode(self.owner_command.load(Ordering::Acquire))
    }

    pub(super) fn mark_owner_return_waiting(&self) {
        self.owner_command
            .store(OwnerCommand::ReturnWaiting as u8, Ordering::Release);
    }

    pub(super) fn finish_owner_command(&self, result: Result<(), BlockHandoffError>) {
        *self.owner_command_result.lock() = Some(result);
        self.owner_command
            .store(OwnerCommand::None as u8, Ordering::Release);
        self.owner_command_wait.notify_all();
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

    pub(super) fn normal_irq_service_active(&self) -> bool {
        matches!(
            self.phase(),
            ControllerPhase::Running | ControllerPhase::Quiescing
        )
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
            || self.maintenance.state() != crate::maintenance::MaintenanceState::Live
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
        self.request_owner_command(OwnerCommand::Handoff)
    }
}

#[cfg(test)]
mod tests {
    use super::DriverEndpointSlot;
    use crate::block::statistics::BlockIoCounters;

    #[test]
    fn driver_endpoint_lease_releases_the_gate_and_restores_on_error() {
        fn mutate_then_fail(slot: &DriverEndpointSlot<u32>) -> Result<(), ()> {
            let mut lease = slot.lease();
            let slot_guard = slot
                .endpoint
                .try_lock()
                .expect("driver callback must run after the slot gate is released");
            assert!(slot_guard.is_none());
            drop(slot_guard);
            *lease.endpoint_mut() = 42;
            Err(())
        }

        let slot = DriverEndpointSlot::new(41);
        assert_eq!(mutate_then_fail(&slot), Err(()));
        assert_eq!(*slot.endpoint.lock(), Some(42));
    }

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
