//! CPU-local physical clockevent ownership.
//!
//! Logical timer queues publish only an earlier deadline. This state machine
//! is the sole upper layer allowed to translate scheduler and task deadlines
//! into physical one-shot comparator programming.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockEventPhase {
    Offline,
    Idle,
    Armed,
    Firing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockEventAction {
    None,
    Program(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClockEventToken {
    cpu_epoch: u64,
    arm_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalClockEvent {
    phase: ClockEventPhase,
    cpu_epoch: u64,
    arm_generation: u64,
    armed_deadline: Option<u64>,
    requested_deadline: Option<u64>,
}

impl LocalClockEvent {
    pub(crate) const fn offline() -> Self {
        Self {
            phase: ClockEventPhase::Offline,
            cpu_epoch: 0,
            arm_generation: 0,
            armed_deadline: None,
            requested_deadline: None,
        }
    }

    pub(crate) fn online(&mut self, deadline: u64) -> ClockEventAction {
        self.cpu_epoch = self
            .cpu_epoch
            .checked_add(1)
            .expect("clockevent CPU epoch exhausted");
        self.requested_deadline = None;
        self.arm(deadline)
    }

    #[allow(dead_code)]
    pub(crate) fn offline_cpu(&mut self) {
        self.cpu_epoch = self
            .cpu_epoch
            .checked_add(1)
            .expect("clockevent CPU epoch exhausted");
        self.phase = ClockEventPhase::Offline;
        self.armed_deadline = None;
        self.requested_deadline = None;
    }

    /// Publishes a deadline from a logical timer queue.
    ///
    /// A later request never rewrites the current comparator. During IRQ
    /// handling the request is remembered and merged by [`Self::finish_irq`].
    pub(crate) fn request_earlier(&mut self, deadline: u64) -> ClockEventAction {
        match self.phase {
            ClockEventPhase::Offline => ClockEventAction::None,
            ClockEventPhase::Idle => self.arm(deadline),
            ClockEventPhase::Armed => {
                if self.armed_deadline.is_none_or(|armed| deadline < armed) {
                    self.arm(deadline)
                } else {
                    ClockEventAction::None
                }
            }
            ClockEventPhase::Firing => {
                self.requested_deadline = Some(
                    self.requested_deadline
                        .map_or(deadline, |requested| requested.min(deadline)),
                );
                ClockEventAction::None
            }
        }
    }

    /// Consumes the currently armed edge before logical timer queues run.
    pub(crate) fn claim_irq(&mut self) -> Option<ClockEventToken> {
        if self.phase != ClockEventPhase::Armed {
            return None;
        }
        self.phase = ClockEventPhase::Firing;
        self.armed_deadline = None;
        Some(ClockEventToken {
            cpu_epoch: self.cpu_epoch,
            arm_generation: self.arm_generation,
        })
    }

    /// Completes one IRQ transaction and selects at most one new comparator.
    pub(crate) fn finish_irq(
        &mut self,
        token: ClockEventToken,
        next_deadline: Option<u64>,
    ) -> ClockEventAction {
        if self.phase != ClockEventPhase::Firing
            || token.cpu_epoch != self.cpu_epoch
            || token.arm_generation != self.arm_generation
        {
            return ClockEventAction::None;
        }
        let deadline = match (self.requested_deadline.take(), next_deadline) {
            (Some(requested), Some(next)) => Some(requested.min(next)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        if let Some(deadline) = deadline {
            self.arm(deadline)
        } else {
            self.phase = ClockEventPhase::Idle;
            ClockEventAction::None
        }
    }

    fn arm(&mut self, deadline: u64) -> ClockEventAction {
        self.arm_generation = self
            .arm_generation
            .checked_add(1)
            .expect("clockevent arm generation exhausted");
        self.phase = ClockEventPhase::Armed;
        self.armed_deadline = Some(deadline);
        ClockEventAction::Program(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::{ClockEventAction, ClockEventPhase, LocalClockEvent};

    #[test]
    fn later_request_does_not_rewrite_an_armed_comparator() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(event.online(100), ClockEventAction::Program(100));
        assert_eq!(event.request_earlier(120), ClockEventAction::None);
        assert_eq!(event.request_earlier(80), ClockEventAction::Program(80));
    }

    #[test]
    fn loongarch_blocked_vcpu_deadline_preempts_the_periodic_tick() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(event.online(10_000), ClockEventAction::Program(10_000));

        // Models the LoongArch blocked-vCPU deadline arriving after the
        // periodic scheduler tick has already armed the local comparator.
        assert_eq!(
            event.request_earlier(2_000),
            ClockEventAction::Program(2_000)
        );
    }

    #[test]
    fn request_during_firing_is_merged_with_recomputed_deadline() {
        let mut event = LocalClockEvent::offline();
        event.online(100);
        let token = event.claim_irq().unwrap();
        assert_eq!(event.request_earlier(140), ClockEventAction::None);
        assert_eq!(
            event.finish_irq(token, Some(160)),
            ClockEventAction::Program(140)
        );
    }

    #[test]
    fn stale_epoch_cannot_rearm_an_onlined_cpu() {
        let mut event = LocalClockEvent::offline();
        event.online(100);
        let stale = event.claim_irq().unwrap();
        event.offline_cpu();
        event.online(200);
        assert_eq!(event.finish_irq(stale, Some(50)), ClockEventAction::None);
    }

    #[test]
    fn no_deadline_leaves_the_clockevent_idle() {
        let mut event = LocalClockEvent::offline();
        event.online(100);
        let token = event.claim_irq().unwrap();
        assert_eq!(event.finish_irq(token, None), ClockEventAction::None);
        assert_eq!(event.phase, ClockEventPhase::Idle);
        assert_eq!(event.request_earlier(200), ClockEventAction::Program(200));
    }

    #[test]
    fn duplicate_or_stale_irq_edges_cannot_replace_the_live_arm() {
        let mut event = LocalClockEvent::offline();
        event.online(100);
        let token = event.claim_irq().unwrap();
        assert_eq!(event.claim_irq(), None);
        assert_eq!(
            event.finish_irq(token, Some(200)),
            ClockEventAction::Program(200)
        );
        assert_eq!(event.finish_irq(token, Some(50)), ClockEventAction::None);
        assert_eq!(event.request_earlier(150), ClockEventAction::Program(150));
    }
}
