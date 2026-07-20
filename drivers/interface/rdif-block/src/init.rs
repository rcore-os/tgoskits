use crate::{BlockIrqSource, IdList};

/// One invocation input for a portable controller initialization state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitInput {
    /// Current absolute monotonic time.
    pub now_ns: u64,
    /// Logical sources whose initialization IRQ endpoint acknowledged an event.
    pub irq_sources: IdList,
}

impl InitInput {
    pub const fn new(now_ns: u64, irq_sources: IdList) -> Self {
        Self {
            now_ns,
            irq_sources,
        }
    }

    pub const fn at(now_ns: u64) -> Self {
        Self::new(now_ns, IdList::none())
    }
}

/// Conditions under which an OS runtime should invoke initialization again.
///
/// Pending work without an activation condition cannot be constructed outside
/// this crate:
///
/// ```compile_fail
/// use rdif_block::{IdList, InitSchedule};
///
/// let _invalid = InitSchedule {
///     run_again: false,
///     irq_sources: IdList::none(),
///     wake_at_ns: None,
/// };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitSchedule {
    /// Pure in-memory transitions remain and should be queued behind the
    /// current bounded worker pass.
    run_again: bool,
    /// Logical command/data IRQ sources that can advance this state.
    irq_sources: IdList,
    /// Absolute monotonic deadline for reset/clock/power/OCR/PHY progress or
    /// failure detection.
    wake_at_ns: Option<u64>,
}

impl InitSchedule {
    /// Creates and validates a set of future initialization activations.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::NoWakeCondition`] when all activation conditions
    /// are absent.
    pub const fn new(
        run_again: bool,
        irq_sources: IdList,
        wake_at_ns: Option<u64>,
    ) -> Result<Self, InitError> {
        Self {
            run_again,
            irq_sources,
            wake_at_ns,
        }
        .validate()
    }

    /// Validates that a pending state names at least one future activation.
    ///
    /// Safe constructors already guarantee this invariant. Runtime boundaries
    /// should still call this method before arming work so an internal driver
    /// defect is reported instead of becoming a permanently lost wakeup.
    pub const fn validate(self) -> Result<Self, InitError> {
        if !self.run_again && self.irq_sources.is_empty() && self.wake_at_ns.is_none() {
            return Err(InitError::NoWakeCondition);
        }
        Ok(self)
    }

    /// Whether pure in-memory state remains ready for another bounded pass.
    pub const fn run_again(&self) -> bool {
        self.run_again
    }

    /// Logical acknowledged IRQ sources that may activate the next pass.
    pub const fn irq_sources(&self) -> IdList {
        self.irq_sources
    }

    /// Absolute monotonic deadline that may activate the next pass.
    pub const fn wake_at_ns(&self) -> Option<u64> {
        self.wake_at_ns
    }

    /// Requests another bounded worker pass after the current pass yields.
    pub const fn immediate() -> Self {
        Self {
            run_again: true,
            irq_sources: IdList::none(),
            wake_at_ns: None,
        }
    }

    /// Waits for one of the named acknowledged logical IRQ sources.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::NoWakeCondition`] when `irq_sources` is empty.
    pub const fn wait_for_irq(irq_sources: IdList) -> Result<Self, InitError> {
        Self::new(false, irq_sources, None)
    }

    /// Waits until either an acknowledged IRQ source or an absolute monotonic
    /// deadline activates the state machine.
    pub const fn wait_for_irq_until(
        irq_sources: IdList,
        wake_at_ns: u64,
    ) -> Result<Self, InitError> {
        Self::new(false, irq_sources, Some(wake_at_ns))
    }

    /// Waits until the specified absolute monotonic deadline.
    pub const fn wait_until(wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            irq_sources: IdList::none(),
            wake_at_ns: Some(wake_at_ns),
        }
    }
}

/// Result of one bounded initialization state-machine invocation.
#[derive(Debug)]
pub enum InitPoll<T> {
    Ready(T),
    Pending(InitSchedule),
    /// Terminal failure after bounded recovery could not continue.
    ///
    /// A hardware state machine must first stop every DMA engine it started.
    /// If the hardware does not acknowledge that stop by its absolute
    /// deadline, the OS must retain the controller, mappings, and DMA backing
    /// in an offline quarantine rather than destructing live ownership.
    Failed(InitError),
}

/// Portable controller initialization or recovery failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InitError {
    #[error("controller initialization has no future wake condition")]
    NoWakeCondition,
    #[error("controller initialization received an invalid state transition")]
    InvalidState,
    #[error("controller initialization timed out")]
    TimedOut,
    #[error("controller initialization requires an unavailable interrupt source")]
    MissingInterrupt,
    #[error("controller initialization failed: {0}")]
    Hardware(&'static str),
}

/// OS-independent reset, discovery, and activation state machine.
///
/// Implementations must perform bounded work, must not sleep or busy-wait, and
/// must express every future invocation through [`InitSchedule`]. The OS binds
/// worker and IRQ capabilities before the first call. Queue capacity must not
/// be published until this trait returns [`InitPoll::Ready`]. A terminal
/// [`InitPoll::Failed`] must obey its DMA-quiescence or quarantine contract.
pub trait ControllerInit {
    type Ready;

    fn poll_init(&mut self, input: InitInput) -> InitPoll<Self::Ready>;
}

/// Object-safe discovery-to-ready state machine retained by an OS runtime.
///
/// Unlike [`ControllerInit`], this endpoint erases the ready value because the
/// queue geometry remains owned by [`crate::Interface`]. The runtime first
/// takes and binds every declared IRQ handler, enables OS-side actions, and
/// only then calls [`Self::poll_init`]. A driver must therefore keep discovery
/// side-effect free: reset, identify, queue-creation, and every other hardware
/// command belong in this bounded state machine.
///
/// Each accepted command must name its next activation through
/// [`InitSchedule`]. The endpoint must not sleep, busy-wait, or inspect a
/// completion register in response to a deadline. A normal-I/O queue may be
/// exposed only after this endpoint returns [`InitPoll::Ready`].
pub trait InitialController: Send {
    /// Logical IRQ sources needed before the controller can become ready.
    fn irq_sources(&self) -> IdList;

    /// Transfers the hard-IRQ endpoint for one declared logical source.
    ///
    /// The runtime calls this exactly once per source before the first
    /// [`Self::poll_init`] invocation. The endpoint must acknowledge the
    /// device source and publish stable facts in the hard-IRQ capture call.
    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource>;

    /// Advances at most one bounded initialization pass.
    fn poll_init(&mut self, input: InitInput) -> InitPoll<()>;
}

/// Discovery-time initialization capability exposed by a block interface.
///
/// `Ready` is the compatibility path for controllers whose discovery object
/// already represents a fully initialized device, including inline software
/// devices. New hardware drivers should expose `Pending` and defer their first
/// hardware command until the runtime has bound the endpoint's IRQ sources.
pub enum ControllerInitEndpoint<'a> {
    Ready,
    Pending(&'a mut dyn InitialController),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum State {
        Reset,
        Clock,
        Command,
        Ready,
    }

    struct FakeController {
        state: State,
        reset_deadline: u64,
        command_source: usize,
    }

    impl ControllerInit for FakeController {
        type Ready = u64;

        fn poll_init(&mut self, input: InitInput) -> InitPoll<Self::Ready> {
            match self.state {
                State::Reset if input.now_ns < self.reset_deadline => {
                    InitPoll::Pending(InitSchedule::wait_until(self.reset_deadline))
                }
                State::Reset => {
                    self.state = State::Clock;
                    InitPoll::Pending(InitSchedule::immediate())
                }
                State::Clock => {
                    self.state = State::Command;
                    let mut sources = IdList::none();
                    sources.insert(self.command_source);
                    InitPoll::Pending(InitSchedule::wait_for_irq(sources).unwrap())
                }
                State::Command if input.irq_sources.contains(self.command_source) => {
                    self.state = State::Ready;
                    InitPoll::Ready(4096)
                }
                State::Command => {
                    let mut sources = IdList::none();
                    sources.insert(self.command_source);
                    InitPoll::Pending(InitSchedule::wait_for_irq(sources).unwrap())
                }
                State::Ready => InitPoll::Failed(InitError::InvalidState),
            }
        }
    }

    fn run(call_times: &[u64]) -> (State, Option<u64>) {
        let mut controller = FakeController {
            state: State::Reset,
            reset_deadline: 100,
            command_source: 3,
        };
        let mut ready = None;
        for &now_ns in call_times {
            let mut sources = IdList::none();
            if now_ns >= 150 {
                sources.insert(3);
            }
            if let InitPoll::Ready(capacity) = controller.poll_init(InitInput::new(now_ns, sources))
            {
                ready = Some(capacity);
                break;
            }
        }
        (controller.state, ready)
    }

    #[test]
    fn initialization_result_is_independent_of_poll_frequency() {
        assert_eq!(run(&[0, 100, 100, 150]), (State::Ready, Some(4096)));
        assert_eq!(
            run(&[0, 1, 2, 25, 99, 100, 100, 120, 149, 150]),
            (State::Ready, Some(4096))
        );
    }

    #[test]
    fn pending_state_must_name_a_future_activation() {
        assert_eq!(
            InitSchedule::new(false, IdList::none(), None),
            Err(InitError::NoWakeCondition)
        );
    }

    #[test]
    fn validation_rejects_an_internally_forged_schedule_without_a_wake_condition() {
        let schedule = InitSchedule {
            run_again: false,
            irq_sources: IdList::none(),
            wake_at_ns: None,
        };

        assert_eq!(schedule.validate(), Err(InitError::NoWakeCondition));
    }

    #[test]
    fn irq_and_deadline_can_be_armed_together() {
        let mut sources = IdList::none();
        sources.insert(1);
        let schedule = InitSchedule::wait_for_irq_until(sources, 100).unwrap();
        assert!(!schedule.run_again());
        assert!(schedule.irq_sources().contains(1));
        assert_eq!(schedule.wake_at_ns(), Some(100));
    }
}
