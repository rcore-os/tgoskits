//! Shared VirtIO PCI transport state.

mod capability;
mod interrupt;
mod transport;

pub use capability::{
    VIRTIO_PCI_CAP_VENDOR_SPECIFIC, VIRTIO_PCI_CONFIG_EFFECT_ID, VirtioPciCapability,
    VirtioPciCapabilitySet, VirtioPciCapabilityType,
};
pub use interrupt::{InterruptReadOutcome, InterruptTransition, VirtioPciInterruptCoordinator};
pub use transport::{
    ActivityPermit, InterruptPublicationRequest, InterruptTransitionIntent,
    InterruptTransitionRequest, QueueNotification, QueueNotifyOutcome, VirtioDeviceCore,
    VirtioPciTransport, VirtioPciWriteOutcome, VirtioQueueGeneration,
};
