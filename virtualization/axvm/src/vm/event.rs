//! Per-vCPU guest-event wait channels.
//!
//! VM lifecycle coordination remains on the runtime's shared wait queue.
//! These channels isolate guest WFI wakeups so an interrupt for one vCPU does
//! not make sibling vCPU tasks runnable.

use core::sync::atomic::{AtomicUsize, Ordering};

/// One runtime-lifetime wait channel for a configured vCPU.
pub(crate) struct VcpuEventChannel {
    wait_queue: crate::WaitQueue,
    generation: AtomicUsize,
}

impl VcpuEventChannel {
    pub(crate) const fn new() -> Self {
        Self {
            wait_queue: crate::WaitQueue::new(),
            generation: AtomicUsize::new(0),
        }
    }

    pub(crate) fn wait(&self) {
        self.wait_queue.wait();
    }

    pub(crate) fn wait_until(&self, condition: impl Fn() -> bool) {
        self.wait_queue.wait_until(condition);
    }

    /// Publishes an event before making this channel's waiter runnable.
    pub(crate) fn notify(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        self.wait_queue.notify_one(false);
    }
}

mod wait_race {
    use super::*;

    /// Generation observed immediately before a vCPU checks whether WFI can sleep.
    pub(crate) struct VcpuEventWaitSnapshot {
        generation: usize,
    }

    impl VcpuEventChannel {
        pub(crate) fn snapshot(&self) -> VcpuEventWaitSnapshot {
            VcpuEventWaitSnapshot {
                generation: self.generation.load(Ordering::Acquire),
            }
        }
    }

    impl VcpuEventWaitSnapshot {
        pub(crate) fn has_pending_event(&self, channel: &VcpuEventChannel) -> bool {
            channel.generation.load(Ordering::Acquire) != self.generation
        }
    }

    /// Blocks only if the VM and target channel remained idle across setup.
    pub(crate) fn wait_for_vcpu_event_if_idle(
        channel: &VcpuEventChannel,
        wait_snapshot: &VcpuEventWaitSnapshot,
        may_wait: impl Fn() -> bool,
        wait_until: impl FnOnce(&dyn Fn() -> bool),
    ) {
        wait_for_vcpu_event_if_idle_with(channel, wait_snapshot, may_wait, || false, wait_until);
    }

    /// Blocks only if the target channel and any additional source remain idle.
    pub(crate) fn wait_for_vcpu_event_if_idle_with(
        channel: &VcpuEventChannel,
        wait_snapshot: &VcpuEventWaitSnapshot,
        may_wait: impl Fn() -> bool,
        additional_ready: impl Fn() -> bool,
        wait_until: impl FnOnce(&dyn Fn() -> bool),
    ) {
        let wake_condition =
            || !may_wait() || wait_snapshot.has_pending_event(channel) || additional_ready();
        if wake_condition() {
            return;
        }
        wait_until(&wake_condition);
    }
}

pub(crate) use wait_race::{wait_for_vcpu_event_if_idle, wait_for_vcpu_event_if_idle_with};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_notification_does_not_advance_a_sibling_channel() {
        let target = VcpuEventChannel::new();
        let sibling = VcpuEventChannel::new();
        let target_snapshot = target.snapshot();
        let sibling_snapshot = sibling.snapshot();

        target.notify();

        assert!(target_snapshot.has_pending_event(&target));
        assert!(!sibling_snapshot.has_pending_event(&sibling));
    }

    #[test]
    fn publication_before_wait_prevents_sleep() {
        let channel = VcpuEventChannel::new();
        let snapshot = channel.snapshot();
        channel.notify();
        let waits = core::cell::Cell::new(0);

        wait_for_vcpu_event_if_idle(&channel, &snapshot, || true, |_| waits.set(waits.get() + 1));

        assert_eq!(waits.get(), 0);
    }

    #[test]
    fn additional_completion_before_wait_prevents_sleep() {
        let channel = VcpuEventChannel::new();
        let snapshot = channel.snapshot();
        let waits = core::cell::Cell::new(0);

        wait_for_vcpu_event_if_idle_with(
            &channel,
            &snapshot,
            || true,
            || true,
            |_| waits.set(waits.get() + 1),
        );

        assert_eq!(waits.get(), 0);
    }

    #[test]
    fn additional_completion_is_rechecked_at_the_park_boundary() {
        let channel = VcpuEventChannel::new();
        let snapshot = channel.snapshot();
        let completed = core::cell::Cell::new(false);

        wait_for_vcpu_event_if_idle_with(
            &channel,
            &snapshot,
            || true,
            || completed.get(),
            |condition| {
                completed.set(true);
                assert!(condition());
            },
        );
    }
}
