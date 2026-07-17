//! Bounded discovery-to-ready VirtIO controller initialization.

use virtio_drivers::{
    Error as VirtIoError,
    queue::VirtQueue,
    transport::{DeviceStatus, DeviceType},
};

use super::{VIRTIO_BLK_QUEUE_ID, device::VirtIoBlkInner, queue::VIRTIO_BLK_QUEUE_SIZE};
use crate::virtio::{VirtIoHalImpl, VirtIoTransport};

const VIRTIO_BLK_CONFIG_CAPACITY_LOW: usize = 0;
const VIRTIO_BLK_CONFIG_CAPACITY_HIGH: usize = 4;
pub(super) const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_F_RING_INDIRECT_DESC: u64 = 1 << 28;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTIO_BLK_SUPPORTED_FEATURES: u64 =
    VIRTIO_BLK_F_RO | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_F_VERSION_1;
const VIRTIO_BLK_INIT_TIMEOUT_NS: u64 = 1_000_000_000;
const VIRTIO_BLK_CONFIG_RETRY_NS: u64 = 50_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VirtioBlockInitPhase {
    Discovered,
    ResetWait,
    FeatureNegotiation,
    CapacitySnapshotStart,
    CapacitySnapshotFinish,
    QueueSetup,
    DriverReady,
    Ready,
    FailureReset,
    Failed,
}

impl<T: VirtIoTransport> VirtIoBlkInner<T> {
    pub(super) fn poll_init(
        &mut self,
        input: rdif_block::InitInput,
        irq_enabled: bool,
    ) -> rdif_block::InitPoll<()> {
        let progress = match self.init_phase {
            VirtioBlockInitPhase::Discovered => self.begin_reset(input.now_ns),
            VirtioBlockInitPhase::ResetWait => self.poll_reset(input.now_ns),
            VirtioBlockInitPhase::FeatureNegotiation => self.negotiate_features(),
            VirtioBlockInitPhase::CapacitySnapshotStart => self.begin_capacity_snapshot(),
            VirtioBlockInitPhase::CapacitySnapshotFinish => {
                self.finish_capacity_snapshot(input.now_ns)
            }
            VirtioBlockInitPhase::QueueSetup => self.install_queue(irq_enabled),
            VirtioBlockInitPhase::DriverReady => {
                if let Err(error) = self.validate_retained_configuration() {
                    rdif_block::InitPoll::Failed(error)
                } else {
                    self.transport.finish_init();
                    self.init_phase = VirtioBlockInitPhase::Ready;
                    rdif_block::InitPoll::Ready(())
                }
            }
            VirtioBlockInitPhase::Ready => {
                rdif_block::InitPoll::Failed(rdif_block::InitError::InvalidState)
            }
            VirtioBlockInitPhase::FailureReset => {
                return self.poll_failure_reset(input.now_ns);
            }
            VirtioBlockInitPhase::Failed => {
                return rdif_block::InitPoll::Failed(
                    self.init_error
                        .unwrap_or(rdif_block::InitError::InvalidState),
                );
            }
        };
        if let rdif_block::InitPoll::Failed(error) = progress {
            return self.begin_failure_reset(error, input.now_ns);
        }
        progress
    }

    fn begin_reset(&mut self, now_ns: u64) -> rdif_block::InitPoll<()> {
        if self.transport.device_type() != DeviceType::Block {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::Hardware(
                "virtio transport is not a block device",
            ));
        }
        self.transport.set_status(DeviceStatus::empty());
        self.init_deadline_ns = now_ns.saturating_add(VIRTIO_BLK_INIT_TIMEOUT_NS);
        self.init_phase = VirtioBlockInitPhase::ResetWait;
        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(next_config_check(
            now_ns,
            self.init_deadline_ns,
        )))
    }

    fn poll_reset(&mut self, now_ns: u64) -> rdif_block::InitPoll<()> {
        if self.transport.get_status().is_empty() {
            self.init_phase = VirtioBlockInitPhase::FeatureNegotiation;
            return rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate());
        }
        if now_ns >= self.init_deadline_ns {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut);
        }
        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(next_config_check(
            now_ns,
            self.init_deadline_ns,
        )))
    }

    fn negotiate_features(&mut self) -> rdif_block::InitPoll<()> {
        self.transport
            .set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER);
        let device_features = self.transport.read_device_features();
        self.negotiated_features = device_features & VIRTIO_BLK_SUPPORTED_FEATURES;
        self.transport
            .write_driver_features(self.negotiated_features);
        self.transport.set_status(
            DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK,
        );
        if !self
            .transport
            .get_status()
            .contains(DeviceStatus::FEATURES_OK)
        {
            return rdif_block::InitPoll::Failed(rdif_block::InitError::Hardware(
                "virtio device rejected negotiated block features",
            ));
        }
        self.transport
            .set_guest_page_size(virtio_drivers::PAGE_SIZE as u32);
        self.init_phase = VirtioBlockInitPhase::CapacitySnapshotStart;
        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
    }

    fn begin_capacity_snapshot(&mut self) -> rdif_block::InitPoll<()> {
        self.config_generation = self.transport.read_config_generation();
        match self
            .transport
            .read_config_space::<u32>(VIRTIO_BLK_CONFIG_CAPACITY_LOW)
        {
            Ok(capacity_low) => {
                self.capacity_low = capacity_low;
                self.init_phase = VirtioBlockInitPhase::CapacitySnapshotFinish;
                rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
            }
            Err(error) => rdif_block::InitPoll::Failed(map_virtio_init_error(error)),
        }
    }

    fn finish_capacity_snapshot(&mut self, now_ns: u64) -> rdif_block::InitPoll<()> {
        let capacity_high = match self
            .transport
            .read_config_space::<u32>(VIRTIO_BLK_CONFIG_CAPACITY_HIGH)
        {
            Ok(capacity_high) => capacity_high,
            Err(error) => return rdif_block::InitPoll::Failed(map_virtio_init_error(error)),
        };
        if self.transport.read_config_generation() != self.config_generation {
            if now_ns >= self.init_deadline_ns {
                return rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut);
            }
            self.init_phase = VirtioBlockInitPhase::CapacitySnapshotStart;
            return rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(
                next_config_check(now_ns, self.init_deadline_ns),
            ));
        }

        self.capacity = u64::from(self.capacity_low) | (u64::from(capacity_high) << 32);
        self.init_phase = VirtioBlockInitPhase::QueueSetup;
        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
    }

    fn install_queue(&mut self, irq_enabled: bool) -> rdif_block::InitPoll<()> {
        let queue = match VirtQueue::<VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>::new(
            &mut self.transport,
            VIRTIO_BLK_QUEUE_ID as u16,
            self.negotiated_features & VIRTIO_F_RING_INDIRECT_DESC != 0,
            // The public VirtQueue API can suppress used notifications through
            // avail.flags only when EVENT_IDX is not negotiated. Recovery must
            // mask device-side notifications before reset, so keep it disabled
            // until the dependency exposes a safe used_event update operation.
            false,
        ) {
            Ok(queue) => queue,
            Err(error) => return rdif_block::InitPoll::Failed(map_virtio_init_error(error)),
        };
        self.queue = Some(queue);
        self.set_interrupts(irq_enabled);
        self.init_phase = VirtioBlockInitPhase::DriverReady;
        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
    }

    fn begin_failure_reset(
        &mut self,
        error: rdif_block::InitError,
        now_ns: u64,
    ) -> rdif_block::InitPoll<()> {
        self.init_error = Some(error);
        self.set_interrupts(false);
        self.transport.set_status(DeviceStatus::empty());
        self.init_deadline_ns = now_ns.saturating_add(VIRTIO_BLK_INIT_TIMEOUT_NS);
        self.init_phase = VirtioBlockInitPhase::FailureReset;
        self.poll_failure_reset(now_ns)
    }

    fn poll_failure_reset(&mut self, now_ns: u64) -> rdif_block::InitPoll<()> {
        if self.transport.get_status().is_empty() {
            // Device status zero acknowledges that no virtqueue remains live;
            // only now may its DMA allocation be released.
            drop(self.queue.take());
            self.init_phase = VirtioBlockInitPhase::Failed;
            return rdif_block::InitPoll::Failed(
                self.init_error
                    .unwrap_or(rdif_block::InitError::InvalidState),
            );
        }
        if now_ns >= self.init_deadline_ns {
            self.quarantine_unproven_dma();
            self.init_error = Some(rdif_block::InitError::TimedOut);
            self.init_phase = VirtioBlockInitPhase::Failed;
            return rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut);
        }
        rdif_block::InitPoll::Pending(rdif_block::InitSchedule::wait_until(next_config_check(
            now_ns,
            self.init_deadline_ns,
        )))
    }

    pub(super) fn quarantine_unproven_dma(&mut self) {
        // A reset timeout cannot justify dropping DMA-visible queue/request
        // storage. The runtime also retains the failed controller; leaking
        // these bounded allocations keeps any late device write memory-safe.
        let descriptor_may_be_live = self.inflight.is_some();
        if let Some(queue) = self.queue.take() {
            core::mem::forget(queue);
        }
        if let Some(inflight) = self.inflight.take() {
            core::mem::forget(inflight);
        }
        if descriptor_may_be_live && let Some(storage) = self.descriptor_storage.take() {
            core::mem::forget(storage);
        }
    }

    pub(super) fn set_interrupts(&mut self, enabled: bool) {
        if let Some(queue) = self.queue.as_mut() {
            queue.set_dev_notify(enabled);
        }
    }

    pub(super) fn finish_reset_after_acknowledgement(&mut self) -> bool {
        if !self.transport.get_status().is_empty() {
            return false;
        }

        // Device status zero means no virtqueue remains live. Dropping the old
        // queue allocation is therefore safe even when its last descriptor did
        // not reach the used ring; request DMA ownership remains in `inflight`
        // until the runtime presents the matching DmaQuiesced proof.
        drop(self.queue.take());
        true
    }

    pub(super) fn prepare_reinitialize(&mut self) -> Result<(), rdif_block::InitError> {
        if self.queue.is_some()
            || self.inflight.is_some()
            || !self.transport.get_status().is_empty()
        {
            return Err(rdif_block::InitError::InvalidState);
        }

        self.init_phase = VirtioBlockInitPhase::Discovered;
        self.negotiated_features = 0;
        self.config_generation = 0;
        self.capacity_low = 0;
        self.capacity = 0;
        self.init_deadline_ns = 0;
        self.init_error = None;
        Ok(())
    }

    pub(super) fn validate_retained_configuration(&mut self) -> Result<(), rdif_block::InitError> {
        let read_only = self.negotiated_features & VIRTIO_BLK_F_RO != 0;
        match (self.retained_capacity, self.retained_read_only) {
            (None, None) => {
                self.retained_capacity = Some(self.capacity);
                self.retained_read_only = Some(read_only);
                Ok(())
            }
            (Some(capacity), Some(retained_read_only))
                if capacity == self.capacity && retained_read_only == read_only =>
            {
                Ok(())
            }
            _ => Err(rdif_block::InitError::Hardware(
                "virtio block geometry changed across controller reset",
            )),
        }
    }
}

fn next_config_check(now_ns: u64, deadline_ns: u64) -> u64 {
    now_ns
        .saturating_add(VIRTIO_BLK_CONFIG_RETRY_NS)
        .min(deadline_ns)
}

fn map_virtio_init_error(error: VirtIoError) -> rdif_block::InitError {
    let message = match error {
        VirtIoError::QueueFull => "virtio block init queue is full",
        VirtIoError::NotReady => "virtio block device is not ready",
        VirtIoError::WrongToken => "virtio block init observed a wrong queue token",
        VirtIoError::AlreadyUsed => "virtio block queue is already active",
        VirtIoError::InvalidParam => "virtio block queue geometry is invalid",
        VirtIoError::DmaError => "virtio block queue DMA allocation failed",
        VirtIoError::IoError => "virtio block config access failed",
        VirtIoError::Unsupported => "virtio block feature is unsupported",
        VirtIoError::ConfigSpaceTooSmall => "virtio block config space is too small",
        VirtIoError::ConfigSpaceMissing => "virtio block config space is missing",
        VirtIoError::SocketDeviceError(_) => "virtio transport returned a socket error",
    };
    rdif_block::InitError::Hardware(message)
}
