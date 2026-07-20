//! Activation and ownership-transfer failures for runtime block controllers.

use alloc::{string::String, vec::Vec};

use rdif_block::{BlkError, BundleError, InitError, LogicalDeviceId, QueueContractError};
use thiserror::Error;

use crate::block::{HardwareQueueError, HostPhysicalRangeError, StorageGuestKey};

/// A controller activation failure that prevents device publication.
#[derive(Debug, Error)]
pub enum BlockControllerError {
    /// Probe metadata contained a malformed host resource interval.
    #[error("block controller {controller} exposed an invalid host resource: {source}")]
    InvalidHostResource {
        controller: String,
        source: HostPhysicalRangeError,
    },
    /// Controller discovery or logical-device materialization failed.
    #[error(transparent)]
    Bundle(#[from] BundleError),
    /// Queue IDs are fixed-width event identifiers and must be unique.
    #[error("block controller queue ID {0} is duplicated or outside 0..64")]
    InvalidQueueId(usize),
    /// Shutdown-lifetime ownership could not be reserved before queue transfer.
    #[error("block queue quarantine capacity is exhausted")]
    QuarantineCapacity,
    /// Portable materialization failed and one or more queue close attempts
    /// had to be transferred into the runtime quarantine registry.
    #[error(
        "logical block device {device_id:?} ({device_name}) failed contract validation \
         ({contract}); rollback failed ({close_error}) and quarantined {queue_count} queue(s)"
    )]
    UnpublishedQueuesQuarantined {
        device_id: LogicalDeviceId,
        device_name: String,
        contract: QueueContractError,
        close_error: BlkError,
        queue_count: usize,
    },
    /// A queue's declared logical IRQ contract is incomplete.
    #[error(transparent)]
    QueueContract(#[from] QueueContractError),
    /// Driver metadata named an IRQ source without a platform binding.
    #[error("block controller logical IRQ source {0} has no platform binding")]
    MissingIrqBinding(usize),
    /// Driver metadata named an IRQ source without a hard-IRQ endpoint.
    #[error("block controller logical IRQ source {0} has no handler")]
    MissingIrqHandler(usize),
    /// Queue materialization and action registration observed different source
    /// maps, so the runtime cannot prove which endpoint owns each queue.
    #[error("block controller IRQ topology changed while actions were being bound")]
    IrqTopologyChanged,
    /// The dynamic platform IRQ domain rejected resolution or registration.
    #[error("block controller IRQ operation failed: {0:?}")]
    Irq(ax_hal::irq::IrqError),
    /// Device-side interrupt masking or unmasking failed.
    #[error("block controller interrupt transition failed: {0}")]
    Driver(BlkError),
    /// Hardware queue activation failed before publication.
    #[error(transparent)]
    HardwareQueue(#[from] HardwareQueueError),
    /// The portable discovery-to-ready state machine failed.
    #[error("block controller initialization failed: {0}")]
    Initialization(InitError),
    /// The fixed maintenance owner could not register, wait, or close safely.
    #[error("block controller maintenance failed: {0}")]
    Maintenance(#[from] crate::maintenance::MaintenanceError),
    /// The activating task could not park until the initialization result.
    #[error("block controller initialization wait failed: {0:?}")]
    Task(crate::task::TaskError),
}

/// Failure of the ordered host-to-guest block ownership transaction.
#[derive(Debug, Error)]
pub enum BlockHandoffError {
    /// A hardware controller has no physical resource identity to match.
    #[error("block controller {controller} has no host MMIO resource identity")]
    MissingResourceIdentity { controller: String },
    /// A guest can reach only part of a controller's register ownership set.
    #[error("guest {owner:?} covers only part of block controller {controller} host resources")]
    PartialResourceCoverage {
        controller: String,
        owner: StorageGuestKey,
    },
    /// More than one guest mapping reaches the same hardware controller.
    #[error("block controller {controller} is reachable by multiple guests {owners:?}")]
    AmbiguousGuestOwners {
        controller: String,
        owners: Vec<StorageGuestKey>,
    },
    /// A controller was not in the host-running state.
    #[error("block controller {0} cannot begin another handoff")]
    InvalidState(String),
    /// A hardware queue failed to drain or detach safely.
    #[error(transparent)]
    HardwareQueue(#[from] HardwareQueueError),
    /// Device-side interrupt masking failed.
    #[error("block handoff could not mask device interrupts: {0}")]
    Driver(#[from] BlkError),
    /// The portable controller lifecycle could not prove DMA ownership.
    #[error("block handoff controller lifecycle failed: {0}")]
    Lifecycle(#[from] InitError),
    /// Platform IRQ disable or synchronization failed.
    #[error("block handoff IRQ operation failed: {0:?}")]
    Irq(ax_hal::irq::IrqError),
    /// The fixed maintenance owner rejected an IRQ action transition.
    #[error("block handoff maintenance operation failed: {0}")]
    Maintenance(#[from] crate::maintenance::MaintenanceError),
    /// Waiting for in-progress filesystem block operations failed.
    #[error(transparent)]
    Task(#[from] crate::task::TaskError),
    /// Guest-owned hardware could not complete the proof-gated return path.
    #[error("block controller {0} could not return from guest ownership")]
    GuestReturn(String),
}

impl From<ax_hal::irq::IrqError> for BlockHandoffError {
    fn from(value: ax_hal::irq::IrqError) -> Self {
        Self::Irq(value)
    }
}

impl From<ax_hal::irq::IrqError> for BlockControllerError {
    fn from(value: ax_hal::irq::IrqError) -> Self {
        Self::Irq(value)
    }
}

impl From<BlkError> for BlockControllerError {
    fn from(value: BlkError) -> Self {
        Self::Driver(value)
    }
}
