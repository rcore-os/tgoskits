//! Runtime-neutral activation contract for SD/MMC initialization.

use crate::Error;

/// IRQ event acknowledged before one initialization state-machine pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitIrqEvent {
    /// No controller event was acknowledged.
    None,
    /// The controller's initialization IRQ endpoint acknowledged progress.
    Controller,
}

/// One bounded invocation of the SD/MMC initialization state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitInput {
    /// Current absolute monotonic time.
    pub now_ns: u64,
    /// IRQ event acknowledged since the previous invocation.
    pub irq: InitIrqEvent,
}

impl InitInput {
    /// Construct an invocation without an IRQ event.
    pub const fn at(now_ns: u64) -> Self {
        Self {
            now_ns,
            irq: InitIrqEvent::None,
        }
    }

    /// Construct an invocation caused by the controller IRQ endpoint.
    pub const fn with_controller_irq(now_ns: u64) -> Self {
        Self {
            now_ns,
            irq: InitIrqEvent::Controller,
        }
    }

    pub(super) const fn has_controller_irq(self) -> bool {
        matches!(self.irq, InitIrqEvent::Controller)
    }

    /// Convert an RDIF controller-init input using an explicitly selected
    /// logical source. Other sources remain outside this SD/MMC instance.
    #[cfg(feature = "rdif")]
    pub fn from_rdif(input: rdif_block::InitInput, controller_source: usize) -> Self {
        if input.irq_sources.contains(controller_source) {
            Self::with_controller_irq(input.now_ns)
        } else {
            Self::at(input.now_ns)
        }
    }
}

/// IRQ source an initialization request is waiting for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitIrqWait {
    /// No IRQ can advance the current state.
    None,
    /// The controller's initialization IRQ can advance the current state.
    Controller,
}

/// Conditions under which initialization should be invoked again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitSchedule {
    /// Pure in-memory work remains and should be queued behind the current
    /// bounded worker pass.
    pub run_again: bool,
    /// IRQ source that can advance the request.
    pub irq: InitIrqWait,
    /// Absolute monotonic wake time. Depending on the state this is either a
    /// progress check for an eventless init phase or a hard failure deadline.
    pub wake_at_ns: Option<u64>,
}

impl InitSchedule {
    /// Schedule another bounded in-memory transition.
    pub const fn immediate() -> Self {
        Self {
            run_again: true,
            irq: InitIrqWait::None,
            wake_at_ns: None,
        }
    }

    /// Wait for an eventless initialization phase until an absolute time.
    pub const fn wait_until(wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            irq: InitIrqWait::None,
            wake_at_ns: Some(wake_at_ns),
        }
    }

    /// Wait for a controller IRQ, with an absolute watchdog deadline.
    pub const fn wait_for_controller_irq(deadline_ns: u64) -> Self {
        Self {
            run_again: false,
            irq: InitIrqWait::Controller,
            wake_at_ns: Some(deadline_ns),
        }
    }

    /// Convert this schedule to RDIF using an explicitly selected logical
    /// controller source.
    #[cfg(feature = "rdif")]
    pub fn into_rdif(
        self,
        controller_source: usize,
    ) -> Result<rdif_block::InitSchedule, rdif_block::InitError> {
        let mut sources = rdif_block::IdList::none();
        if matches!(self.irq, InitIrqWait::Controller) {
            sources.insert(controller_source);
        }
        rdif_block::InitSchedule::new(self.run_again, sources, self.wake_at_ns)
    }
}

/// Result of one bounded SD/MMC initialization invocation.
#[derive(Debug)]
pub enum InitPoll<T> {
    /// Initialization completed and the card may be published.
    Ready(T),
    /// Initialization remains active under the returned schedule.
    Pending(InitSchedule),
    /// Initialization failed. The controller must be recovered before reuse.
    Failed(Error),
}
