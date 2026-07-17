//! One controller owning one or more independent logical block devices.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::{any::Any, fmt, num::NonZeroUsize};

use crate::{
    BInterface, BIrqHandler, BlkError, CompletedRequest, CompletionSink, ControllerInitEndpoint,
    DeviceInfo, DriverGeneric, IrqSourceList, LifecycleEndpoint, QueueContractError, QueueHandle,
    QueueLimits, validate_queue_info,
};

/// Maximum number of logical devices described by one fixed-width bundle set.
pub const MAX_LOGICAL_DEVICES: usize = u64::BITS as usize;
/// Maximum number of controller-global queue identities representable by RDIF.
pub const MAX_CONTROLLER_QUEUES: usize = u64::BITS as usize;

/// Controller-local identity of one independently addressed block device.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LogicalDeviceId(u8);

impl LogicalDeviceId {
    /// Creates an identity representable by [`LogicalDeviceIds`].
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::InvalidDeviceId`] for values outside `0..64`.
    pub const fn new(value: usize) -> Result<Self, BundleError> {
        if value < MAX_LOGICAL_DEVICES {
            Ok(Self(value as u8))
        } else {
            Err(BundleError::InvalidDeviceId { value })
        }
    }

    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// Fixed, allocation-free set of controller-local logical device identities.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct LogicalDeviceIds(u64);

impl LogicalDeviceIds {
    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, id: LogicalDeviceId) -> bool {
        self.0 & (1_u64 << id.get()) != 0
    }

    pub fn insert(&mut self, id: LogicalDeviceId) {
        self.0 |= 1_u64 << id.get();
    }

    pub fn remove(&mut self, id: LogicalDeviceId) {
        self.0 &= !(1_u64 << id.get());
    }

    pub fn iter(self) -> impl Iterator<Item = LogicalDeviceId> {
        (0..MAX_LOGICAL_DEVICES)
            .filter(move |index| self.0 & (1_u64 << index) != 0)
            .map(|index| LogicalDeviceId(index as u8))
    }
}

/// Materialized queues and immutable geometry for one logical block device.
///
/// Queue IDs remain controller-wide because shared IRQ snapshots route by that
/// identity. The queue collection itself belongs only to this device, so a
/// runtime cannot select a sibling disk's queue for the same logical address
/// space.
pub struct LogicalDevice {
    id: LogicalDeviceId,
    name: String,
    device_info: DeviceInfo,
    queue_limits: QueueLimits,
    queues: Vec<QueueHandle>,
}

impl LogicalDevice {
    pub fn new(
        id: LogicalDeviceId,
        name: String,
        device_info: DeviceInfo,
        queue_limits: QueueLimits,
        queues: Vec<QueueHandle>,
    ) -> Self {
        Self {
            id,
            name,
            device_info,
            queue_limits,
            queues,
        }
    }

    pub const fn id(&self) -> LogicalDeviceId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn device_info(&self) -> DeviceInfo {
        self.device_info
    }

    pub const fn queue_limits(&self) -> QueueLimits {
        self.queue_limits
    }

    pub fn queue_count(&self) -> usize {
        self.queues.len()
    }

    pub fn queues(&self) -> impl ExactSizeIterator<Item = &QueueHandle> {
        self.queues.iter()
    }

    pub fn into_parts(self) -> LogicalDeviceParts {
        LogicalDeviceParts {
            id: self.id,
            name: self.name,
            device_info: self.device_info,
            queue_limits: self.queue_limits,
            queues: self.queues,
        }
    }
}

impl fmt::Debug for LogicalDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogicalDevice")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("device_info", &self.device_info)
            .field("queue_limits", &self.queue_limits)
            .field("queue_count", &self.queues.len())
            .finish()
    }
}

/// Owned decomposition consumed by an OS block runtime.
pub struct LogicalDeviceParts {
    pub id: LogicalDeviceId,
    pub name: String,
    pub device_info: DeviceInfo,
    pub queue_limits: QueueLimits,
    pub queues: Vec<QueueHandle>,
}

/// Controller-bundle discovery or materialization failure.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum BundleError {
    #[error("logical block device ID {value} is outside 0..64")]
    InvalidDeviceId { value: usize },
    #[error("logical block device {device_id:?} is unavailable or was already extracted")]
    DeviceUnavailable { device_id: LogicalDeviceId },
    #[error(
        "controller returned logical block device {returned:?} while {requested:?} was requested"
    )]
    UnexpectedDeviceId {
        requested: LogicalDeviceId,
        returned: LogicalDeviceId,
    },
    #[error(
        "controller logical-device set changed during extraction: expected {expected_bits:#x}, \
         got {actual_bits:#x}"
    )]
    DeviceSetChanged {
        expected_bits: u64,
        actual_bits: u64,
    },
    #[error("logical block device {device_id:?} exposed no queue")]
    NoQueues { device_id: LogicalDeviceId },
    #[error(
        "logical block device {device_id:?} exceeded its materialization limit of {max_queues} \
         queues"
    )]
    QueueLimitExceeded {
        device_id: LogicalDeviceId,
        max_queues: usize,
    },
    #[error("logical block device queue contract is invalid: {0}")]
    QueueContract(#[from] QueueContractError),
    #[error("logical block device materialization failed: {0}")]
    Driver(#[from] BlkError),
}

/// Portable ownership root for a controller with independent logical devices.
///
/// Initialization, IRQ endpoints, device-side masking, DMA lifecycle, and the
/// proof cookie are controller-wide. Device extraction happens only after
/// initialization and returns materialized queue collections that must remain
/// isolated by the consuming runtime.
pub trait ControllerBundle: DriverGeneric {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_>;

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_>;

    /// Returns logical devices that are ready and have not been extracted.
    fn logical_device_ids(&self) -> LogicalDeviceIds;

    /// Materializes one logical device and at most `max_queues` queues.
    ///
    /// Queue identities are controller-global even though the returned queue
    /// collection belongs only to `device_id`.
    fn take_logical_device(
        &mut self,
        device_id: LogicalDeviceId,
        max_queues: NonZeroUsize,
    ) -> Result<LogicalDevice, BundleError>;

    fn enable_irq(&self) -> Result<(), BlkError>;

    fn disable_irq(&self) -> Result<(), BlkError>;

    fn is_irq_enabled(&self) -> bool;

    fn irq_sources(&self) -> IrqSourceList;

    fn take_irq_handler(&mut self, source_id: usize) -> Option<BIrqHandler>;
}

pub type BControllerBundle = Box<dyn ControllerBundle>;

/// Explicit compatibility adapter for a legacy one-device [`Interface`].
///
/// The adapter materializes every queue into one [`LogicalDevice`]. It never
/// treats several unrelated devices as interchangeable queues. Failed queue
/// validation explicitly shuts down every unpublished queue before allowing a
/// retry; a shutdown failure makes the adapter permanently unavailable.
pub struct SingleDeviceBundle {
    interface: BInterface,
    available: bool,
}

impl SingleDeviceBundle {
    pub const fn new(interface: BInterface) -> Self {
        Self {
            interface,
            available: true,
        }
    }

    fn extract_queues(&mut self, max_queues: NonZeroUsize) -> Vec<QueueHandle> {
        let mut queues = Vec::new();
        for _ in 0..max_queues.get() {
            let Some(queue) = self.interface.create_queue() else {
                break;
            };
            queues.push(queue);
        }
        queues
    }
}

impl DriverGeneric for SingleDeviceBundle {
    fn name(&self) -> &str {
        self.interface.name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl ControllerBundle for SingleDeviceBundle {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        self.interface.controller_init()
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.interface.lifecycle()
    }

    fn logical_device_ids(&self) -> LogicalDeviceIds {
        if self.available {
            LogicalDeviceIds::from_bits(1)
        } else {
            LogicalDeviceIds::none()
        }
    }

    fn take_logical_device(
        &mut self,
        device_id: LogicalDeviceId,
        max_queues: NonZeroUsize,
    ) -> Result<LogicalDevice, BundleError> {
        let expected = LogicalDeviceId(0);
        if !self.available || device_id != expected {
            return Err(BundleError::DeviceUnavailable { device_id });
        }
        if max_queues.get() > MAX_CONTROLLER_QUEUES {
            return Err(BundleError::QueueLimitExceeded {
                device_id,
                max_queues: MAX_CONTROLLER_QUEUES,
            });
        }

        let name = self.interface.name().to_string();
        let device_info = self.interface.device_info();
        let queue_limits = self.interface.queue_limits();
        let queues = self.extract_queues(max_queues);
        if queues.is_empty() {
            return Err(BundleError::NoQueues { device_id });
        }
        let logical_device = LogicalDevice::new(device_id, name, device_info, queue_limits, queues);
        if let Err(error) = validate_controller_devices(core::slice::from_ref(&logical_device)) {
            if let Err(shutdown_error) = shutdown_unpublished_logical_device(logical_device) {
                self.available = false;
                return Err(shutdown_error.into());
            }
            return Err(error.into());
        }

        self.available = false;
        Ok(logical_device)
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        self.interface.enable_irq()
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.interface.disable_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.interface.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.interface.irq_sources()
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<BIrqHandler> {
        self.interface.take_irq_handler(source_id)
    }
}

fn shutdown_unpublished_logical_device(device: LogicalDevice) -> Result<(), BlkError> {
    let mut first_error = None;
    let mut sink = UnpublishedCompletionSink;
    for mut queue in device.into_parts().queues {
        if let Err(error) = queue.shutdown(&mut sink)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

struct UnpublishedCompletionSink;

impl CompletionSink for UnpublishedCompletionSink {
    fn complete(&mut self, completion: CompletedRequest) {
        // No request can be accepted before materialization. A driver that
        // returns ownership here has violated that boundary, so keep it
        // fail-closed instead of freeing potentially device-visible storage.
        core::mem::forget(completion);
    }
}

/// Validates device isolation and controller-global queue identities.
///
/// # Errors
///
/// Rejects empty bundles, duplicate device/queue identities, empty device
/// queue sets, or queue metadata that names a different logical address space.
pub fn validate_controller_devices(devices: &[LogicalDevice]) -> Result<(), QueueContractError> {
    if devices.is_empty() {
        return Err(QueueContractError::MissingLogicalDevices);
    }

    let mut device_ids = LogicalDeviceIds::none();
    let mut queue_ids = crate::IdList::none();
    for device in devices {
        if device_ids.contains(device.id) {
            return Err(QueueContractError::DuplicateLogicalDeviceId {
                device_id: device.id.get(),
            });
        }
        device_ids.insert(device.id);
        if device.queues.is_empty() {
            return Err(QueueContractError::LogicalDeviceHasNoQueues {
                device_id: device.id.get(),
            });
        }
        for queue in &device.queues {
            let info = queue.info();
            if queue.id() != info.id {
                return Err(QueueContractError::QueueIdentityMismatch {
                    advertised_id: queue.id(),
                    metadata_id: info.id,
                });
            }
            validate_queue_info(info)?;
            if queue_ids.contains(info.id) {
                return Err(QueueContractError::DuplicateControllerQueueId { queue_id: info.id });
            }
            queue_ids.insert(info.id);
            if info.device != device.device_info || info.limits != device.queue_limits {
                return Err(QueueContractError::QueueDeviceMetadataMismatch {
                    device_id: device.id.get(),
                    queue_id: info.id,
                });
            }
        }
    }
    Ok(())
}
