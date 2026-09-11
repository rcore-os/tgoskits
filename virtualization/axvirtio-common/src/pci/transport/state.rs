use alloc::{format, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axdevice_base::{DeviceError, DeviceResult};

use super::VirtioQueueGeneration;
use crate::{
    NoGuestMemoryAccessor, VirtioQueue,
    constants::{
        VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DEVICE_NEEDS_RESET, VIRTIO_STATUS_DRIVER,
        VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FAILED, VIRTIO_STATUS_FEATURES_OK,
    },
};

const DRIVER_PHASE_BITS: u8 = (VIRTIO_STATUS_ACKNOWLEDGE
    | VIRTIO_STATUS_DRIVER
    | VIRTIO_STATUS_FEATURES_OK
    | VIRTIO_STATUS_DRIVER_OK) as u8;
const DRIVER_STATUS_BITS: u8 = DRIVER_PHASE_BITS | VIRTIO_STATUS_FAILED as u8;
const KNOWN_STATUS_BITS: u8 = DRIVER_STATUS_BITS | VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8;

#[derive(Debug)]
pub(super) struct QueueActivity {
    pub(super) accepting: AtomicBool,
    pub(super) active: AtomicUsize,
    pub(super) resetting: AtomicBool,
}

impl QueueActivity {
    pub(super) const fn new() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            resetting: AtomicBool::new(false),
        }
    }

    pub(super) fn begin_reset(&self) -> bool {
        self.resetting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        generation: VirtioQueueGeneration,
    ) -> Option<ActivityPermit> {
        if !self.accepting.load(Ordering::Acquire) {
            return None;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if self.accepting.load(Ordering::Acquire) {
            Some(ActivityPermit {
                activity: Arc::clone(self),
                generation,
            })
        } else {
            self.active.fetch_sub(1, Ordering::AcqRel);
            None
        }
    }

    pub(super) fn close_and_drain(&self) -> bool {
        self.accepting.store(false, Ordering::Release);
        for _ in 0..super::RESET_DRAIN_SPIN_LIMIT {
            if self.active.load(Ordering::Acquire) == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    pub(super) fn reopen(&self) {
        self.accepting.store(true, Ordering::Release);
    }

    pub(super) fn finish_reset(&self) {
        self.resetting.store(false, Ordering::Release);
    }
}

/// Permit covering synchronous queue activity through completion publication.
#[derive(Debug)]
pub struct ActivityPermit {
    pub(super) activity: Arc<QueueActivity>,
    pub(super) generation: VirtioQueueGeneration,
}

impl Drop for ActivityPermit {
    fn drop(&mut self) {
        self.activity.active.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Debug)]
pub(super) struct QueueState {
    pub(super) queue: VirtioQueue<NoGuestMemoryAccessor>,
    pub(super) enabled: bool,
    pub(super) processing: bool,
}

pub(super) struct TransportState {
    pub(super) device_feature_select: u32,
    pub(super) driver_feature_select: u32,
    pub(super) driver_features: u64,
    pub(super) status: u8,
    pub(super) queue_select: u16,
    pub(super) queue_size: u16,
    pub(super) queues: alloc::vec::Vec<QueueState>,
    pub(super) queue_generation: u64,
    pub(super) config_generation: u8,
    pub(super) fault_reported: bool,
    pub(super) device_needs_reset: bool,
    pub(super) reset_pending: bool,
}

impl TransportState {
    pub(super) fn new(queue_num_max: u16, queue_size_max: u16) -> Self {
        let queues = (0..queue_num_max)
            .map(|index| QueueState {
                queue: VirtioQueue::new(index, queue_size_max, Arc::new(NoGuestMemoryAccessor)),
                enabled: false,
                processing: false,
            })
            .collect();
        Self {
            device_feature_select: 0,
            driver_feature_select: 0,
            driver_features: 0,
            status: 0,
            queue_select: 0,
            queue_size: queue_size_max,
            queues,
            queue_generation: 0,
            config_generation: 0,
            fault_reported: false,
            device_needs_reset: false,
            reset_pending: false,
        }
    }

    pub(super) fn reset(&mut self, queue_size_max: u16) {
        self.device_feature_select = 0;
        self.driver_feature_select = 0;
        self.driver_features = 0;
        self.status = VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8;
        self.queue_select = 0;
        self.queue_size = queue_size_max;
        self.queue_generation = self.queue_generation.wrapping_add(1);
        self.config_generation = self.config_generation.wrapping_add(1);
        self.fault_reported = false;
        self.device_needs_reset = true;
        self.reset_pending = true;
        for queue in &mut self.queues {
            queue.enabled = false;
            queue.processing = false;
            queue.queue.reset();
        }
    }

    pub(super) fn write_driver_status(
        &mut self,
        requested: u8,
        device_features: u64,
    ) -> DeviceResult {
        if requested & !KNOWN_STATUS_BITS != 0 {
            return Err(DeviceError::InvalidInput {
                operation: "virtio-pci status",
                detail: format!("unknown status bits: {requested:#x}"),
            });
        }

        let current_driver = self.status & DRIVER_STATUS_BITS;
        let requested_driver = requested & DRIVER_STATUS_BITS;
        if current_driver & !requested_driver != 0 {
            return Err(DeviceError::InvalidState {
                operation: "update virtio-pci status",
                detail: format!(
                    "nonzero status writes cannot clear driver bits: {current_driver:#x} -> \
                     {requested_driver:#x}"
                ),
            });
        }
        if current_driver & VIRTIO_STATUS_FAILED as u8 != 0 && requested_driver != current_driver {
            return Err(DeviceError::InvalidState {
                operation: "update virtio-pci status",
                detail: "FAILED status requires a device reset before further progress".into(),
            });
        }

        validate_driver_status_phase(requested_driver)?;

        let device_status = if self.device_needs_reset {
            VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8
        } else {
            0
        };
        let mut accepted_driver = requested_driver;
        if current_driver & VIRTIO_STATUS_FEATURES_OK as u8 == 0
            && requested_driver & VIRTIO_STATUS_FEATURES_OK as u8 != 0
            && self.driver_features & !device_features != 0
        {
            // The device reports rejected feature negotiation by withholding
            // FEATURES_OK. FAILED remains owned by the driver.
            accepted_driver &= !(VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) as u8;
        }

        self.status = accepted_driver | device_status;
        Ok(())
    }

    pub(super) fn ensure_feature_negotiation_open(&self) -> DeviceResult {
        if self.status & (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_FAILED) as u8 != 0 {
            Err(DeviceError::InvalidState {
                operation: "update virtio-pci driver features",
                detail: "driver features are frozen after feature negotiation closes".into(),
            })
        } else {
            Ok(())
        }
    }
}

fn validate_driver_status_phase(status: u8) -> DeviceResult {
    let acknowledge = VIRTIO_STATUS_ACKNOWLEDGE as u8;
    let driver = VIRTIO_STATUS_DRIVER as u8;
    let features_ok = VIRTIO_STATUS_FEATURES_OK as u8;
    let driver_ok = VIRTIO_STATUS_DRIVER_OK as u8;
    let valid = status & driver == 0 || status & acknowledge != 0;
    let features_valid =
        status & features_ok == 0 || status & (acknowledge | driver) == acknowledge | driver;
    let driver_ok_valid = status & driver_ok == 0
        || status & (acknowledge | driver | features_ok) == acknowledge | driver | features_ok;
    if valid && features_valid && driver_ok_valid {
        Ok(())
    } else {
        Err(DeviceError::InvalidState {
            operation: "update virtio-pci status",
            detail: format!("driver status phase is out of order: {status:#x}"),
        })
    }
}
