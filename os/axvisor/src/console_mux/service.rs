//! Bounded service of console output, physical input, and guest state.

/// Task-context operations used by the console service loop.
pub trait ConsoleService {
    /// One input item returned to the command interpreter.
    type Input;

    /// Consumes a lifecycle invalidation before reading current guest state.
    fn take_state_change(&mut self) -> bool;
    /// Refreshes guest state and returns a stopped foreground guest to the shell.
    fn reconcile(&mut self);
    /// Consumes one ordered output record or drop report, if available.
    fn try_output(&mut self) -> bool;
    /// Publishes retries for producers whose output submissions were blocked.
    fn flush_output_retries(&mut self);
    /// Consumes at most one input item without blocking.
    fn try_input(&mut self) -> Option<Self::Input>;
    /// Waits without a timer for input, output, or a lifecycle invalidation.
    /// An invalidation published since `take_state_change` must prevent sleep.
    fn wait_event(&mut self);
}

/// Arbitrates console service before input enters a potentially blocking command.
#[derive(Default)]
pub struct ConsolePump;

// Service budgets bound work per turn, independently of the number of guests.
const OUTPUT_BUDGET: usize = 128;

impl ConsolePump {
    /// Services one turn and returns at most one input item.
    pub fn turn<S: ConsoleService>(&mut self, service: &mut S) -> Option<S::Input> {
        if service.take_state_change() {
            service.reconcile();
        }

        let mut output_progress = false;
        for _ in 0..OUTPUT_BUDGET {
            if !service.try_output() {
                break;
            }
            output_progress = true;
        }
        if output_progress {
            // A command can wait for a guest, so retry publication must precede
            // handing input back to its interpreter, including newline bytes.
            service.flush_output_retries();
        }
        let input = service.try_input();
        if input.is_none() && !output_progress {
            service.wait_event();
        }
        input
    }
}

#[cfg(any(test, axtest))]
mod tests {
    use alloc::collections::VecDeque;

    use super::*;

    #[derive(Default)]
    struct ReadyConsole {
        input: VecDeque<u8>,
        output_ready: bool,
        output_count: usize,
        retries_pending: bool,
        retry_batches: usize,
        reconciles: usize,
        state_changed: bool,
        waits: usize,
    }

    impl ConsoleService for ReadyConsole {
        type Input = u8;

        fn take_state_change(&mut self) -> bool {
            core::mem::take(&mut self.state_changed)
        }
        fn reconcile(&mut self) {
            self.reconciles += 1;
        }
        fn try_output(&mut self) -> bool {
            if self.output_ready {
                self.output_count += 1;
                self.retries_pending = true;
            }
            self.output_ready
        }
        fn flush_output_retries(&mut self) {
            self.retry_batches += 1;
            self.retries_pending = false;
        }
        fn try_input(&mut self) -> Option<u8> {
            // A returned command may block: producers must already be runnable.
            assert!(!self.retries_pending);
            self.input.pop_front()
        }
        fn wait_event(&mut self) {
            self.waits += 1;
        }
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn continuous_output_and_input_both_make_progress() {
        let mut service = ReadyConsole {
            input: VecDeque::from(*b"help\n"),
            output_ready: true,
            ..Default::default()
        };
        let mut pump = ConsolePump;
        for expected in b"help\n" {
            let before = service.output_count;
            assert_eq!(pump.turn(&mut service), Some(*expected));
            assert!(service.output_count > before);
            assert!(service.output_count - before <= 128);
        }
        assert_eq!(service.retry_batches, 5);
        assert_eq!(service.waits, 0);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn guest_state_reconciles_only_after_notifications() {
        let mut service = ReadyConsole {
            output_ready: true,
            state_changed: true,
            ..Default::default()
        };
        let mut pump = ConsolePump;
        pump.turn(&mut service);
        pump.turn(&mut service);
        assert_eq!(service.reconciles, 1);
        pump.turn(&mut service);
        assert_eq!(service.reconciles, 1);
        service.state_changed = true;
        pump.turn(&mut service);
        assert_eq!(service.reconciles, 2);
        service.output_ready = false;
        pump.turn(&mut service);
        assert_eq!(service.waits, 1);
        pump.turn(&mut service);
        assert_eq!(service.reconciles, 2);
        // A silent lifecycle publication wakes idle service without I/O.
        service.state_changed = true;
        pump.turn(&mut service);
        assert_eq!(service.reconciles, 3);
    }
}
