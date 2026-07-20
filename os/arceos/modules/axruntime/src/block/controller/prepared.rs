//! Linear preparation transaction for an unpublished block controller.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize};

use ax_driver::block::RdifBlockDevice;
use ax_kspin::SpinNoPreempt;
use rdif_block::{
    IrqSourceInfo, LifecycleEndpoint, validate_lifecycle_activation, validate_lifecycle_identity,
};

use super::{
    BlockController, BlockControllerError, ControllerPhase, DriverEndpointSlot,
    MAX_HARDWARE_QUEUES, RuntimeBlockDevice,
    owner::ControllerOwnerLink,
    recovery::RecoveryStep,
    registry::{
        capture_host_physical_ranges, capture_pci_endpoint, create_runtime_devices,
        materialize_logical_devices, rollback_unpublished_runtime_devices,
        shutdown_logical_device_iter, validate_runtime_devices,
    },
    source::BlockMaintenanceEvent,
};
use crate::{
    block::{
        handoff::{HostPciEndpoint, HostPhysicalRange},
        quarantine::QueueQuarantineReservations,
    },
    maintenance::DeviceMaintenanceHandle,
    task::WaitQueue,
};

/// Unpublished queue topology constructed by the final maintenance owner.
///
/// Portable controllers may discover their normal-I/O interrupt topology
/// while queue endpoints are materialized. This linear object keeps those
/// resources unpublished until every frozen source is registered on the final
/// maintenance CPU and a single commit publishes the complete controller.
pub(in crate::block) struct PreparedBlockController {
    name: String,
    host_physical_ranges: Box<[HostPhysicalRange]>,
    pci_endpoint: Option<HostPciEndpoint>,
    devices: Vec<RuntimeBlockDevice>,
    maintenance: Arc<DeviceMaintenanceHandle<BlockMaintenanceEvent>>,
    lifecycle_cookie: usize,
    owner_link: Arc<ControllerOwnerLink>,
    declared_sources: Vec<IrqSourceInfo>,
}

impl BlockController {
    pub(in crate::block) fn prepare_on_owner(
        device: &mut Option<RdifBlockDevice>,
        maintenance: Arc<DeviceMaintenanceHandle<BlockMaintenanceEvent>>,
    ) -> Result<PreparedBlockController, BlockControllerError> {
        let device_owner = device
            .as_mut()
            .expect("controller preparation requires its unpublished device owner");
        let name = String::from(device_owner.name());
        let host_physical_ranges = capture_host_physical_ranges(device_owner)?;
        let pci_endpoint = capture_pci_endpoint(device_owner);
        // Reserve every possible retention slot before transferring the first
        // portable queue. A bound reservation then follows that queue until
        // explicit close or permanent quarantine.
        let mut quarantine_reservations =
            match QueueQuarantineReservations::reserve(MAX_HARDWARE_QUEUES) {
                Ok(reservations) => reservations,
                Err(_) => return Err(BlockControllerError::QuarantineCapacity),
            };
        let logical_devices =
            materialize_logical_devices(device_owner, &mut quarantine_reservations)?;
        let queue_kinds = logical_devices
            .iter()
            .flat_map(|device| device.queues())
            .map(|queue| queue.info().kind)
            .collect::<Vec<_>>();
        let (lifecycle_kind, lifecycle_cookie) = match device_owner.bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => (rdif_block::LifecycleKind::Inline, 0),
            LifecycleEndpoint::Interrupt(lifecycle) => (
                rdif_block::LifecycleKind::Interrupt,
                lifecycle.controller_cookie(),
            ),
        };
        if let Err(error) = validate_lifecycle_activation(&queue_kinds, lifecycle_kind) {
            shutdown_logical_device_iter(logical_devices.into_iter(), &mut quarantine_reservations);
            return Err(error.into());
        }
        if let Err(error) = validate_lifecycle_identity(lifecycle_kind, lifecycle_cookie) {
            shutdown_logical_device_iter(logical_devices.into_iter(), &mut quarantine_reservations);
            return Err(error.into());
        }
        let owner_link = Arc::new(ControllerOwnerLink::new());
        let devices = create_runtime_devices(
            logical_devices,
            Arc::clone(&maintenance),
            Arc::clone(&owner_link),
            lifecycle_cookie,
            &mut quarantine_reservations,
        )?;
        let declared_sources = device_owner.bundle().irq_sources();
        Ok(PreparedBlockController {
            name,
            host_physical_ranges,
            pci_endpoint,
            devices,
            maintenance,
            lifecycle_cookie,
            owner_link,
            declared_sources,
        })
    }
}

impl PreparedBlockController {
    /// Returns the normal-I/O source topology frozen after queue creation.
    pub(in crate::block) fn declared_sources(&self) -> &[IrqSourceInfo] {
        &self.declared_sources
    }

    /// Commits the unpublished queues after every frozen source is bound.
    pub(in crate::block) fn commit_on_owner(
        self,
        device: &mut Option<RdifBlockDevice>,
        bound_sources: rdif_block::IdList,
    ) -> Result<Arc<BlockController>, BlockControllerError> {
        let device_owner = device
            .as_mut()
            .expect("controller commit requires its unpublished device owner");
        let current_sources = device_owner.bundle().irq_sources();
        if current_sources != self.declared_sources {
            rollback_unpublished_runtime_devices(&self.devices);
            return Err(BlockControllerError::IrqTopologyChanged);
        }
        if let Err(error) =
            validate_runtime_devices(&self.devices, &self.declared_sources, bound_sources)
        {
            rollback_unpublished_runtime_devices(&self.devices);
            return Err(error);
        }

        let device = device
            .take()
            .expect("successful controller commit consumes its device owner");
        let controller = Arc::new(BlockController {
            name: self.name,
            host_physical_ranges: self.host_physical_ranges,
            pci_endpoint: self.pci_endpoint,
            devices: self.devices.into_boxed_slice(),
            device: DriverEndpointSlot::new(device),
            maintenance: self.maintenance,
            maintenance_thread: SpinNoPreempt::new(None),
            owner_command: AtomicU8::new(0),
            owner_command_result: SpinNoPreempt::new(None),
            owner_command_wait: WaitQueue::new(),
            phase: AtomicU8::new(ControllerPhase::Running as u8),
            handoff_reserved: AtomicBool::new(false),
            active_operations: AtomicUsize::new(0),
            operation_wait: WaitQueue::new(),
            recovery_step: AtomicU8::new(RecoveryStep::Idle as u8),
            recovery_deadline_ns: AtomicU64::new(0),
            recovery_epoch: AtomicU64::new(rdif_block::ControllerEpoch::INITIAL.get()),
            irq_recovery_queues: AtomicU64::new(0),
            recovery_cause: SpinNoPreempt::new(None),
            lifecycle_cookie: self.lifecycle_cookie,
            recovery_wait_sources: AtomicU64::new(0),
            recovery_pending_sources: AtomicU64::new(0),
            recovery_irqs_enabled: AtomicBool::new(false),
            owner_link: Arc::clone(&self.owner_link),
        });
        self.owner_link.publish(&controller);
        Ok(controller)
    }

    /// Explicitly rolls back a prepared topology that was never published.
    pub(in crate::block) fn abort(self) {
        rollback_unpublished_runtime_devices(&self.devices);
    }
}
