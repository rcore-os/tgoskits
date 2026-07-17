//! VirtIO device-reset and reinitialization state machine.

use rdif_block::{
    ControllerEpoch, ControllerReady, DmaQuiesced, InitError, InitInput, InitPoll, InitSchedule,
    RecoveryCause,
};

const RESET_ACK_TIMEOUT_NS: u64 = 1_000_000_000;
const RESET_ACK_CHECK_INTERVAL_NS: u64 = 50_000;

/// Narrow controller capability consumed by the lifecycle state machine.
pub(super) trait VirtioLifecycleHardware {
    /// Returns the stable identity used to bind linear lifecycle proofs.
    fn controller_cookie(&self) -> usize;

    /// Writes zero to device status after normal queue and IRQ access is closed.
    fn begin_device_reset(&self);

    /// Completes reset only after status reads back as zero.
    ///
    /// Returning `true` also discards the old virtqueue. The implementation must
    /// not return until the device has acknowledged that the queue is no longer
    /// live and therefore cannot access any exposed request buffer.
    fn finish_reset_after_acknowledgement(&self) -> bool;

    /// Clears retained software state so the staged initializer can rebuild it.
    fn prepare_reinitialize(&self) -> Result<(), InitError>;

    /// Advances the same staged initializer used for first activation.
    fn poll_reinitialize(&self, input: InitInput) -> InitPoll<()>;
}

/// Non-blocking reset and full device reconstruction state.
pub(super) struct VirtioBlockLifecycle {
    state: LifecycleState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    Running,
    GuestOwned,
    Resetting {
        epoch: ControllerEpoch,
        deadline_ns: Option<u64>,
    },
    Quiesced {
        epoch: ControllerEpoch,
    },
    Reinitializing {
        epoch: ControllerEpoch,
    },
    Failed,
}

impl VirtioBlockLifecycle {
    pub(super) const fn running() -> Self {
        Self {
            state: LifecycleState::Running,
        }
    }

    pub(super) const fn can_run(&self) -> bool {
        matches!(self.state, LifecycleState::Running)
    }

    pub(super) fn begin_dma_quiesce(
        &mut self,
        hardware: &impl VirtioLifecycleHardware,
        epoch: ControllerEpoch,
        _cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if !matches!(
            self.state,
            LifecycleState::Running | LifecycleState::GuestOwned
        ) || hardware.controller_cookie() == 0
        {
            return Err(InitError::InvalidState);
        }

        hardware.begin_device_reset();
        self.state = LifecycleState::Resetting {
            epoch,
            deadline_ns: None,
        };
        Ok(())
    }

    pub(super) fn poll_dma_quiesce(
        &mut self,
        hardware: &impl VirtioLifecycleHardware,
        input: InitInput,
    ) -> InitPoll<DmaQuiesced> {
        let LifecycleState::Resetting {
            epoch,
            mut deadline_ns,
        } = self.state
        else {
            return InitPoll::Failed(InitError::InvalidState);
        };

        if hardware.finish_reset_after_acknowledgement() {
            self.state = LifecycleState::Quiesced { epoch };
            // SAFETY: the runtime closes queue admission, masks device IRQs,
            // and drains the registered IRQ action before starting reset. The
            // hardware boundary returns true only after device status reads 0
            // and the old virtqueue has been discarded. Per the VirtIO cleanup
            // contract, no queue is live after acknowledged device reset.
            return InitPoll::Ready(unsafe {
                DmaQuiesced::new(epoch, hardware.controller_cookie())
            });
        }

        let deadline =
            *deadline_ns.get_or_insert_with(|| input.now_ns.saturating_add(RESET_ACK_TIMEOUT_NS));
        if input.now_ns >= deadline {
            return self.fail(InitError::TimedOut);
        }
        self.state = LifecycleState::Resetting { epoch, deadline_ns };
        InitPoll::Pending(reset_ack_schedule(input.now_ns, deadline))
    }

    pub(super) fn enter_guest_owned(
        &mut self,
        hardware: &impl VirtioLifecycleHardware,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        let LifecycleState::Quiesced { epoch } = self.state else {
            return Err(InitError::InvalidState);
        };
        if !proof_matches(hardware, epoch, &quiesced) {
            return Err(InitError::InvalidState);
        }

        self.state = LifecycleState::GuestOwned;
        Ok(())
    }

    pub(super) fn begin_reinitialize(
        &mut self,
        hardware: &impl VirtioLifecycleHardware,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        let LifecycleState::Quiesced { epoch } = self.state else {
            return Err(InitError::InvalidState);
        };
        if !proof_matches(hardware, epoch, &quiesced) {
            return Err(InitError::InvalidState);
        }
        if let Err(error) = hardware.prepare_reinitialize() {
            self.state = LifecycleState::Failed;
            return Err(error);
        }

        self.state = LifecycleState::Reinitializing { epoch };
        Ok(())
    }

    pub(super) fn poll_reinitialize(
        &mut self,
        hardware: &impl VirtioLifecycleHardware,
        input: InitInput,
    ) -> InitPoll<ControllerReady> {
        let LifecycleState::Reinitializing { epoch } = self.state else {
            return InitPoll::Failed(InitError::InvalidState);
        };

        match hardware.poll_reinitialize(input) {
            InitPoll::Ready(()) => {
                self.state = LifecycleState::Running;
                // SAFETY: the shared staged initializer negotiated features,
                // read a stable configuration snapshot, created a fresh queue,
                // programmed its interrupt state, and set DRIVER_OK only after
                // the matching DmaQuiesced proof was consumed.
                InitPoll::Ready(unsafe {
                    ControllerReady::new(epoch, hardware.controller_cookie())
                })
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => self.fail(error),
        }
    }

    fn fail<T>(&mut self, error: InitError) -> InitPoll<T> {
        self.state = LifecycleState::Failed;
        InitPoll::Failed(error)
    }
}

fn proof_matches(
    hardware: &impl VirtioLifecycleHardware,
    epoch: ControllerEpoch,
    quiesced: &DmaQuiesced,
) -> bool {
    quiesced.epoch() == epoch && quiesced.controller_cookie() == hardware.controller_cookie()
}

fn reset_ack_schedule(now_ns: u64, deadline_ns: u64) -> InitSchedule {
    InitSchedule::wait_until(
        now_ns
            .saturating_add(RESET_ACK_CHECK_INTERVAL_NS)
            .min(deadline_ns),
    )
}
