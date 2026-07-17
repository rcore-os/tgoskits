use crate::{InitError, InitInput, InitIrqProgress, InitPoll, QueueContractError, RequestId};

/// Monotonic identity of one controller activation or recovery attempt.
///
/// A runtime advances this value before closing normal queue admission. IRQ
/// snapshots, DMA-quiesce proofs, and reinitialization proofs from an older
/// epoch must never be applied to the new controller state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ControllerEpoch(u64);

impl ControllerEpoch {
    /// Initial epoch of a newly published controller queue.
    ///
    /// A DMA-quiescence proof must describe a strictly later epoch before it
    /// can reclaim ownership from that queue.
    pub const INITIAL: Self = Self(1);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Reason why the runtime closed admission and entered controller recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryCause {
    /// Driver or runtime queue invariants no longer permit normal service.
    QueueFault {
        /// Driver-local hardware queue identity.
        queue_id: usize,
    },
    /// An absolute request watchdog expired without completion IRQ evidence.
    Timeout {
        /// Driver-local hardware queue identity.
        queue_id: usize,
        /// Exact generation-bearing request identity.
        request_id: RequestId,
    },
    /// Task-context cancellation won while the device still owned the request.
    Cancelled {
        /// Driver-local hardware queue identity.
        queue_id: usize,
        /// Exact generation-bearing request identity.
        request_id: RequestId,
    },
    /// The fixed IRQ event snapshot ring could not retain another event.
    EventOverflow {
        /// Driver-local hardware queue identity.
        queue_id: usize,
    },
    /// Host ownership is being quiesced for an exclusive device handoff.
    Handoff,
}

/// Linear proof that the named controller epoch can no longer access DMA.
///
/// This value is intentionally not `Copy` or constructible through safe code.
/// The runtime must retain it until every accepted queue request has returned
/// ownership or transfer it into [`InterruptLifecycle::begin_reinitialize`].
///
/// Safe code cannot fabricate the proof because its representation is private:
///
/// ```compile_fail
/// use rdif_block::{ControllerEpoch, DmaQuiesced};
///
/// fn forge(epoch: ControllerEpoch) -> DmaQuiesced {
///     DmaQuiesced { epoch, controller_cookie: 1 }
/// }
/// ```
///
/// The proof is linear, so one quiescence result cannot authorize both guest
/// handoff and host reinitialization:
///
/// ```compile_fail
/// use rdif_block::{DmaQuiesced, InitError, InterruptLifecycle};
///
/// fn reuse(
///     lifecycle: &mut dyn InterruptLifecycle,
///     proof: DmaQuiesced,
/// ) -> Result<(), InitError> {
///     lifecycle.enter_guest_owned(proof)?;
///     lifecycle.begin_reinitialize(proof)
/// }
/// ```
#[derive(Debug, Eq, PartialEq)]
#[must_use = "DMA ownership may only be reclaimed or transferred with this proof"]
pub struct DmaQuiesced {
    epoch: ControllerEpoch,
    controller_cookie: usize,
}

impl DmaQuiesced {
    /// Creates a controller-bound DMA-quiescence proof.
    ///
    /// # Safety
    ///
    /// The caller must have masked device interrupt generation, drained the
    /// corresponding OS IRQ actions, stopped bus mastering and every device
    /// DMA engine for `epoch`, and prevented any queue from publishing new
    /// descriptors. `controller_cookie` must uniquely identify that retained
    /// controller instance for its entire runtime lifetime.
    pub const unsafe fn new(epoch: ControllerEpoch, controller_cookie: usize) -> Self {
        Self {
            epoch,
            controller_cookie,
        }
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.epoch
    }

    pub const fn controller_cookie(&self) -> usize {
        self.controller_cookie
    }
}

/// Linear proof that one controller epoch has been fully reconstructed.
///
/// Queue capacity and normal IRQ delivery may only be republished after this
/// proof matches the runtime-owned epoch and controller identity.
///
/// ```compile_fail
/// use rdif_block::{ControllerEpoch, ControllerReady};
///
/// fn forge(epoch: ControllerEpoch) -> ControllerReady {
///     ControllerReady { epoch, controller_cookie: 1 }
/// }
/// ```
#[derive(Debug, Eq, PartialEq)]
#[must_use = "a controller may only be republished after validating this proof"]
pub struct ControllerReady {
    epoch: ControllerEpoch,
    controller_cookie: usize,
}

impl ControllerReady {
    /// Creates a controller-bound ready proof.
    ///
    /// # Safety
    ///
    /// The caller must have completed the controller's full initialization
    /// state machine for `epoch`, including power/clock/reset, queue memory,
    /// DMA, interrupt status, and device-side interrupt mask programming. No
    /// stale command, event, or DMA operation from an older epoch may remain.
    /// `controller_cookie` must identify the same controller that supplied the
    /// consumed [`DmaQuiesced`] proof.
    pub const unsafe fn new(epoch: ControllerEpoch, controller_cookie: usize) -> Self {
        Self {
            epoch,
            controller_cookie,
        }
    }

    pub const fn epoch(&self) -> ControllerEpoch {
        self.epoch
    }

    pub const fn controller_cookie(&self) -> usize {
        self.controller_cookie
    }
}

/// Runtime-visible lifecycle class of one block controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleKind {
    Inline,
    Interrupt,
}

/// Borrowed lifecycle endpoint supplied by a block controller interface.
pub enum LifecycleEndpoint<'controller> {
    /// Pure software device with no asynchronous hardware ownership.
    Inline,
    /// Interrupt-backed controller with explicit DMA recovery states.
    Interrupt(&'controller mut dyn InterruptLifecycle),
}

impl LifecycleEndpoint<'_> {
    pub const fn kind(&self) -> LifecycleKind {
        match self {
            Self::Inline => LifecycleKind::Inline,
            Self::Interrupt(_) => LifecycleKind::Interrupt,
        }
    }
}

/// Validates the stable identity used to bind linear controller proofs.
///
/// Inline devices do not issue DMA lifecycle proofs. Interrupt-backed devices
/// must publish a nonzero identity that remains stable until shutdown.
pub const fn validate_lifecycle_identity(
    kind: LifecycleKind,
    controller_cookie: usize,
) -> Result<(), QueueContractError> {
    if matches!(kind, LifecycleKind::Interrupt) && controller_cookie == 0 {
        return Err(QueueContractError::InvalidLifecycleIdentity);
    }
    Ok(())
}

/// OS-independent recovery and reinitialization state machine.
///
/// Each poll must perform bounded work, must not sleep or busy-wait, and must
/// express its next activation through [`crate::InitSchedule`]. Normal queue
/// completion status may only be consumed from acknowledged IRQ snapshots;
/// deadlines in this lifecycle detect failure and must not poll for a missed
/// successful completion.
pub trait InterruptLifecycle: Send {
    /// Returns a stable non-zero identity for this retained controller.
    ///
    /// The value is never dereferenced by RDIF. It binds linear lifecycle
    /// proofs to the controller instance that created them and must remain
    /// unchanged until the interface and all of its queues are destroyed.
    fn controller_cookie(&self) -> usize;

    /// Retries one lifecycle IRQ whose hard-IRQ endpoint could not perform its
    /// destructive status read.
    ///
    /// The runtime calls this only from its bounded worker. An implementation
    /// may return [`InitIrqProgress::Acknowledged`] only after it has read and
    /// cleared the device source and cached all state required by the next
    /// [`Self::poll_dma_quiesce`] or [`Self::poll_reinitialize`] call. A
    /// deferred or unhandled source must not be inserted into
    /// [`InitInput::irq_sources`]. [`InitIrqProgress::Failed`] must preserve an
    /// owned-source acknowledgement failure for immediate recovery isolation.
    /// This method must not inspect normal request completion as a polling
    /// fallback.
    fn service_deferred_irq(&mut self, source_id: usize) -> InitIrqProgress;

    /// Starts controller-wide DMA quiescence after queue admission, driver
    /// access, device IRQ generation, and OS IRQ actions have been closed.
    fn begin_dma_quiesce(
        &mut self,
        epoch: ControllerEpoch,
        cause: RecoveryCause,
    ) -> Result<(), InitError>;

    /// Advances bounded DMA quiescence until it returns its linear proof.
    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<DmaQuiesced>;

    /// Consumes the host-side quiescence proof when ownership moves to a guest.
    ///
    /// The lifecycle must retain an explicit `GuestOwned` state so a later
    /// return starts a fresh DMA-quiescence epoch instead of reusing the proof
    /// that preceded guest execution.
    fn enter_guest_owned(&mut self, quiesced: DmaQuiesced) -> Result<(), InitError>;

    /// Consumes DMA ownership proof and starts a full controller rebuild.
    fn begin_reinitialize(&mut self, quiesced: DmaQuiesced) -> Result<(), InitError>;

    /// Advances bounded reinitialization until queue state can be republished.
    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<ControllerReady>;
}
