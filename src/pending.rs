use core::array;

use alloc::collections::vec_deque::VecDeque;

use crate::{SignalInfo, SignalSet};

/// Structure to record pending signals.
pub struct PendingSignals {
    /// The pending signals.
    ///
    /// Note that does not correspond to `pending signals` as described in
    /// Linux. `Pending signals` in Linux refers to the signals that are
    /// delivered but blocked from delivery, while `pending` here refers to any
    /// signal that is delivered and not yet handled.
    pub set: SignalSet,

    /// Signal info of standard signals (1-31).
    info_std: [Option<SignalInfo>; 32],
    /// Signal info queue for real-time signals.
    info_rt: [VecDeque<SignalInfo>; 33],
}
impl PendingSignals {
    pub fn new() -> Self {
        Self {
            set: SignalSet::default(),
            info_std: Default::default(),
            info_rt: array::from_fn(|_| VecDeque::new()),
        }
    }

    /// Puts a signal into the pending queue.
    ///
    /// Returns `true` if the signal was added, `false` if the signal is
    /// standard and ignored (i.e. already pending).
    pub fn put_signal(&mut self, sig: SignalInfo) -> bool {
        let signo = sig.signo();
        let added = self.set.add(signo);

        if signo.is_realtime() {
            self.info_rt[signo as usize - 32].push_back(sig);
        } else {
            if !added {
                // At most one standard signal can be pending.
                return false;
            }
            self.info_std[signo as usize] = Some(sig);
        }
        true
    }

    /// Dequeues the next pending signal contained in `mask`, if any.
    pub fn dequeue_signal(&mut self, mask: &SignalSet) -> Option<SignalInfo> {
        self.set.dequeue(mask).and_then(|signo| {
            if signo.is_realtime() {
                let queue = &mut self.info_rt[signo as usize - 32];
                let result = queue.pop_front();
                if !queue.is_empty() {
                    self.set.add(signo);
                }
                result
            } else {
                self.info_std[signo as usize].take()
            }
        })
    }
}
