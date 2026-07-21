//! Minimal queue-notification capability separated from `Transport`.

use alloc::sync::Arc;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use rdif_block::BlkError;

const MMIO_QUEUE_NOTIFY_OFFSET: usize = 0x50;
const MMIO_QUEUE_NOTIFY_END: usize = MMIO_QUEUE_NOTIFY_OFFSET + size_of::<u32>();
const PCI_COMMON_QUEUE_SELECT_OFFSET: usize = 22;
const PCI_COMMON_QUEUE_NOTIFY_OFF_OFFSET: usize = 30;
const PCI_COMMON_QUEUE_NOTIFY_OFF_END: usize =
    PCI_COMMON_QUEUE_NOTIFY_OFF_OFFSET + size_of::<u16>();

/// Queue-doorbell capability retained by the final I/O-domain owner.
///
/// It exposes no transport configuration, ISR acknowledgement, or queue
/// discovery operations. PCI binding resolves `queue_notify_off` once while
/// the controller and I/O domain still share their fixed owner.
pub struct VirtioQueueNotifyPort {
    registers: VirtioNotifyRegisters,
}

/// Queue notification capability after its immutable queue doorbell is known.
///
/// Keeping this as a distinct type prevents an accepted request from reaching
/// a fallible "doorbell not bound" branch after its descriptor became visible.
pub(super) struct BoundVirtioQueueNotifyPort {
    registers: BoundVirtioNotifyRegisters,
}

enum VirtioNotifyRegisters {
    Mmio {
        mapping: Arc<mmio_api::Mmio>,
    },
    Pci {
        common: Arc<mmio_api::Mmio>,
        notify: Arc<mmio_api::Mmio>,
        multiplier: u32,
    },
    #[cfg(test)]
    Test {
        notifications: Arc<AtomicUsize>,
    },
}

enum BoundVirtioNotifyRegisters {
    Mmio {
        mapping: Arc<mmio_api::Mmio>,
        queue: u16,
    },
    Pci {
        common: Arc<mmio_api::Mmio>,
        notify: Arc<mmio_api::Mmio>,
        multiplier: u32,
        offset: usize,
        queue: u16,
    },
    #[cfg(test)]
    Test { notifications: Arc<AtomicUsize> },
}

impl VirtioQueueNotifyPort {
    pub(super) fn from_mmio(mapping: Arc<mmio_api::Mmio>) -> Result<Self, BlkError> {
        if mapping.size() < MMIO_QUEUE_NOTIFY_END {
            return Err(BlkError::Other(
                "virtio MMIO mapping does not contain the queue notify register",
            ));
        }
        Ok(Self {
            registers: VirtioNotifyRegisters::Mmio { mapping },
        })
    }

    pub(super) fn from_pci(
        common: mmio_api::Mmio,
        notify: mmio_api::Mmio,
        multiplier: u32,
    ) -> Result<Self, BlkError> {
        if common.size() < PCI_COMMON_QUEUE_NOTIFY_OFF_END {
            return Err(BlkError::Other(
                "virtio PCI common capability is too short for queue notification",
            ));
        }
        if notify.size() < size_of::<u16>() {
            return Err(BlkError::Other(
                "virtio PCI notify capability has invalid geometry",
            ));
        }
        Ok(Self {
            registers: VirtioNotifyRegisters::Pci {
                common: Arc::new(common),
                notify: Arc::new(notify),
                multiplier,
            },
        })
    }

    /// Resolves the one queue's immutable doorbell address after queue setup.
    pub(super) fn bind_queue(
        self,
        queue: u16,
    ) -> Result<BoundVirtioQueueNotifyPort, (BlkError, Self)> {
        let registers = match self.registers {
            VirtioNotifyRegisters::Mmio { mapping } => {
                BoundVirtioNotifyRegisters::Mmio { mapping, queue }
            }
            VirtioNotifyRegisters::Pci {
                common,
                notify,
                multiplier,
            } => {
                common.write(PCI_COMMON_QUEUE_SELECT_OFFSET, queue);
                let queue_offset =
                    usize::from(common.read::<u16>(PCI_COMMON_QUEUE_NOTIFY_OFF_OFFSET));
                let offset = match pci_notify_offset(queue_offset as u16, multiplier, notify.size())
                {
                    Ok(offset) => offset,
                    Err(error) => {
                        return Err((
                            error,
                            Self {
                                registers: VirtioNotifyRegisters::Pci {
                                    common,
                                    notify,
                                    multiplier,
                                },
                            },
                        ));
                    }
                };
                BoundVirtioNotifyRegisters::Pci {
                    common,
                    notify,
                    multiplier,
                    offset,
                    queue,
                }
            }
            #[cfg(test)]
            VirtioNotifyRegisters::Test { notifications } => {
                BoundVirtioNotifyRegisters::Test { notifications }
            }
        };
        Ok(BoundVirtioQueueNotifyPort { registers })
    }

    #[cfg(test)]
    pub(super) fn for_test(notifications: Arc<AtomicUsize>) -> Self {
        Self {
            registers: VirtioNotifyRegisters::Test { notifications },
        }
    }
}

impl BoundVirtioQueueNotifyPort {
    /// Rings the queue whose doorbell was resolved before publication.
    pub(super) fn notify(&mut self) {
        match &mut self.registers {
            BoundVirtioNotifyRegisters::Mmio { mapping, queue } => {
                mapping.write(MMIO_QUEUE_NOTIFY_OFFSET, u32::from(*queue));
            }
            BoundVirtioNotifyRegisters::Pci {
                notify,
                offset,
                queue,
                ..
            } => notify.write(*offset, *queue),
            #[cfg(test)]
            BoundVirtioNotifyRegisters::Test { notifications } => {
                notifications.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Returns to the unbound phase after an acknowledged controller reset.
    pub(super) fn into_unbound(self) -> VirtioQueueNotifyPort {
        let registers = match self.registers {
            BoundVirtioNotifyRegisters::Mmio { mapping, .. } => {
                VirtioNotifyRegisters::Mmio { mapping }
            }
            BoundVirtioNotifyRegisters::Pci {
                common,
                notify,
                multiplier,
                ..
            } => VirtioNotifyRegisters::Pci {
                common,
                notify,
                multiplier,
            },
            #[cfg(test)]
            BoundVirtioNotifyRegisters::Test { notifications } => {
                VirtioNotifyRegisters::Test { notifications }
            }
        };
        VirtioQueueNotifyPort { registers }
    }
}

fn pci_notify_offset(
    queue_offset: u16,
    multiplier: u32,
    notify_size: usize,
) -> Result<usize, BlkError> {
    usize::from(queue_offset)
        .checked_mul(multiplier as usize)
        .filter(|offset| {
            offset
                .checked_add(size_of::<u16>())
                .is_some_and(|end| end <= notify_size)
        })
        .ok_or(BlkError::Other(
            "virtio PCI queue notify offset exceeds its capability",
        ))
}

#[cfg(test)]
mod tests {
    use super::pci_notify_offset;

    #[test]
    fn zero_notify_multiplier_maps_every_queue_to_capability_base() {
        assert_eq!(pci_notify_offset(u16::MAX, 0, size_of::<u16>()), Ok(0));
    }

    #[test]
    fn notify_offset_rejects_a_doorbell_outside_the_capability() {
        assert!(pci_notify_offset(2, 4, 10).is_ok());
        assert!(pci_notify_offset(2, 4, 9).is_err());
    }
}
