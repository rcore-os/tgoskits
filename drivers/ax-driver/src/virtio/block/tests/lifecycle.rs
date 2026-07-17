use super::*;

#[test]
fn acknowledged_reset_is_required_before_dma_quiescence_proof() {
    let hardware = FakeLifecycleHardware::new();
    let mut lifecycle = VirtioBlockLifecycle::running();
    let epoch = rdif_block::ControllerEpoch::new(7);

    lifecycle
        .begin_dma_quiesce(&hardware, epoch, rdif_block::RecoveryCause::Handoff)
        .expect("transport status reset must start");
    assert_eq!(hardware.reset_count.get(), 1);
    assert!(!lifecycle.can_run());

    let rdif_block::InitPoll::Pending(schedule) =
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(100))
    else {
        panic!("unacknowledged reset must remain pending")
    };
    assert_eq!(schedule.wake_at_ns(), Some(50_100));
    assert_eq!(hardware.finish_count.get(), 0);

    hardware.reset_acknowledged.set(true);
    let rdif_block::InitPoll::Ready(proof) =
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(1_000))
    else {
        panic!("status zero must prove old virtqueue DMA is stopped")
    };
    assert_eq!(proof.epoch(), epoch);
    assert_eq!(proof.controller_cookie(), FakeLifecycleHardware::COOKIE);
    assert_eq!(hardware.finish_count.get(), 1);
}

#[test]
fn transport_status_zero_drives_the_real_device_lifecycle_boundary() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = VirtIoBlkDevice::discovered(RecordingTransport::new(Arc::clone(&commands)));
    device.with_task(|inner| {
        inner
            .transport
            .set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::DRIVER_OK);
    });
    let mut lifecycle = VirtioBlockLifecycle::running();
    let epoch = rdif_block::ControllerEpoch::new(9);

    lifecycle
        .begin_dma_quiesce(&device, epoch, rdif_block::RecoveryCause::Handoff)
        .unwrap();
    assert!(device.with_task(|inner| inner.transport.get_status().is_empty()));
    let rdif_block::InitPoll::Ready(proof) =
        lifecycle.poll_dma_quiesce(&device, rdif_block::InitInput::at(0))
    else {
        panic!("the real transport boundary must accept status-zero acknowledgement")
    };
    assert_eq!(proof.epoch(), epoch);
    assert_eq!(proof.controller_cookie(), device.controller_cookie());
    assert!(commands.load(Ordering::Relaxed) >= 2);
}

#[test]
fn reset_timeout_is_based_on_absolute_time_not_poll_count() {
    let hardware = FakeLifecycleHardware::new();
    let mut lifecycle = VirtioBlockLifecycle::running();
    lifecycle
        .begin_dma_quiesce(
            &hardware,
            rdif_block::ControllerEpoch::new(10),
            rdif_block::RecoveryCause::Handoff,
        )
        .unwrap();

    assert!(matches!(
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(100)),
        rdif_block::InitPoll::Pending(_)
    ));
    for _ in 0..64 {
        assert!(matches!(
            lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(100)),
            rdif_block::InitPoll::Pending(_)
        ));
    }
    assert!(matches!(
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(1_000_000_099)),
        rdif_block::InitPoll::Pending(_)
    ));
    assert!(matches!(
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(1_000_000_100)),
        rdif_block::InitPoll::Failed(rdif_block::InitError::TimedOut)
    ));
}

#[test]
fn reinitialization_reuses_the_staged_initializer_after_quiescence() {
    let hardware = FakeLifecycleHardware::new();
    hardware.reset_acknowledged.set(true);
    let mut lifecycle = VirtioBlockLifecycle::running();
    let epoch = rdif_block::ControllerEpoch::new(11);
    lifecycle
        .begin_dma_quiesce(&hardware, epoch, rdif_block::RecoveryCause::Handoff)
        .unwrap();
    let rdif_block::InitPoll::Ready(proof) =
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(0))
    else {
        panic!("acknowledged fake reset must quiesce")
    };

    lifecycle.begin_reinitialize(&hardware, proof).unwrap();
    assert_eq!(hardware.prepare_count.get(), 1);
    assert!(matches!(
        lifecycle.poll_reinitialize(&hardware, rdif_block::InitInput::at(10)),
        rdif_block::InitPoll::Pending(_)
    ));

    hardware.reinitialize_ready.set(true);
    let rdif_block::InitPoll::Ready(ready) =
        lifecycle.poll_reinitialize(&hardware, rdif_block::InitInput::at(20))
    else {
        panic!("completed staged initialization must republish the controller")
    };
    assert_eq!(ready.epoch(), epoch);
    assert_eq!(ready.controller_cookie(), FakeLifecycleHardware::COOKIE);
    assert!(lifecycle.can_run());
}

#[test]
fn reinitialization_rejects_changed_published_block_geometry() {
    let commands = Arc::new(AtomicUsize::new(0));
    let device = VirtIoBlkDevice::discovered(RecordingTransport::new(commands));

    device.with_task(|inner| {
        inner.capacity = 4096;
        inner.negotiated_features = VIRTIO_BLK_F_RO;
        inner
            .validate_retained_configuration()
            .expect("first ready configuration establishes the retained geometry");

        inner.capacity = 8192;
        assert_eq!(
            inner.validate_retained_configuration(),
            Err(rdif_block::InitError::Hardware(
                "virtio block geometry changed across controller reset"
            ))
        );
    });
}

#[test]
fn guest_return_requires_a_fresh_reset_epoch() {
    let hardware = FakeLifecycleHardware::new();
    hardware.reset_acknowledged.set(true);
    let mut lifecycle = VirtioBlockLifecycle::running();
    let handoff_epoch = rdif_block::ControllerEpoch::new(17);
    lifecycle
        .begin_dma_quiesce(&hardware, handoff_epoch, rdif_block::RecoveryCause::Handoff)
        .unwrap();
    let rdif_block::InitPoll::Ready(handoff_proof) =
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(0))
    else {
        panic!("handoff reset must quiesce")
    };
    lifecycle
        .enter_guest_owned(&hardware, handoff_proof)
        .unwrap();

    let return_epoch = rdif_block::ControllerEpoch::new(18);
    lifecycle
        .begin_dma_quiesce(&hardware, return_epoch, rdif_block::RecoveryCause::Handoff)
        .unwrap();
    assert_eq!(hardware.reset_count.get(), 2);
    let rdif_block::InitPoll::Ready(return_proof) =
        lifecycle.poll_dma_quiesce(&hardware, rdif_block::InitInput::at(1))
    else {
        panic!("guest return must produce a new proof")
    };
    assert_eq!(return_proof.epoch(), return_epoch);
}

struct FakeLifecycleHardware {
    reset_acknowledged: Cell<bool>,
    reset_count: Cell<usize>,
    finish_count: Cell<usize>,
    prepare_count: Cell<usize>,
    reinitialize_ready: Cell<bool>,
}

impl FakeLifecycleHardware {
    const COOKIE: usize = 0xfeed_1000;

    const fn new() -> Self {
        Self {
            reset_acknowledged: Cell::new(false),
            reset_count: Cell::new(0),
            finish_count: Cell::new(0),
            prepare_count: Cell::new(0),
            reinitialize_ready: Cell::new(false),
        }
    }
}

impl VirtioLifecycleHardware for FakeLifecycleHardware {
    fn controller_cookie(&self) -> usize {
        Self::COOKIE
    }

    fn begin_device_reset(&self) {
        self.reset_count.set(self.reset_count.get() + 1);
    }

    fn finish_reset_after_acknowledgement(&self) -> bool {
        if !self.reset_acknowledged.get() {
            return false;
        }
        self.finish_count.set(self.finish_count.get() + 1);
        true
    }

    fn prepare_reinitialize(&self) -> Result<(), rdif_block::InitError> {
        self.prepare_count.set(self.prepare_count.get() + 1);
        Ok(())
    }

    fn poll_reinitialize(&self, _input: rdif_block::InitInput) -> rdif_block::InitPoll<()> {
        if self.reinitialize_ready.get() {
            rdif_block::InitPoll::Ready(())
        } else {
            rdif_block::InitPoll::Pending(rdif_block::InitSchedule::immediate())
        }
    }
}
