use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
use ax_hal::irq::CpuId;
use ax_hal::irq::IrqError;

const STATE_MASK: usize = 0b11;
const STATE_IDLE: usize = 0;
const STATE_SENDING: usize = 1;
const STATE_ARMED: usize = 2;
const EPOCH_STEP: usize = STATE_MASK + 1;

/// Result of publishing one physical IPI notification edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiNotification {
    /// This publication sent a physical IPI.
    Sent,
    /// An already armed physical IPI covers this publication.
    Coalesced,
}

/// Per-destination physical IPI edge state.
///
/// The epoch prevents a sender whose controller operation completes after the
/// target has already claimed the interrupt from re-arming that old edge over
/// a newer publication.
pub(crate) struct DeliveryEdge {
    state: AtomicUsize,
}

impl DeliveryEdge {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicUsize::new(STATE_IDLE),
        }
    }

    pub(crate) fn notify(
        &self,
        send: impl FnOnce() -> Result<(), IrqError>,
    ) -> Result<IpiNotification, IrqError> {
        let state = &self.state;
        let mut observed = state.load(Ordering::Acquire);

        loop {
            match observed & STATE_MASK {
                STATE_IDLE => {
                    let sending = observed | STATE_SENDING;
                    match state.compare_exchange_weak(
                        observed,
                        sending,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            return match send() {
                                Ok(()) => {
                                    let _ = state.compare_exchange(
                                        sending,
                                        observed | STATE_ARMED,
                                        Ordering::Release,
                                        Ordering::Relaxed,
                                    );
                                    Ok(IpiNotification::Sent)
                                }
                                Err(error) => {
                                    let _ = state.compare_exchange(
                                        sending,
                                        next_idle_epoch(observed),
                                        Ordering::Release,
                                        Ordering::Relaxed,
                                    );
                                    Err(error)
                                }
                            };
                        }
                        Err(actual) => observed = actual,
                    }
                }
                STATE_SENDING => {
                    core::hint::spin_loop();
                    observed = state.load(Ordering::Acquire);
                }
                STATE_ARMED => return Ok(IpiNotification::Coalesced),
                _ => unreachable!("invalid IPI delivery edge state"),
            }
        }
    }

    pub(crate) fn claim(&self) {
        let state = &self.state;
        let mut observed = state.load(Ordering::Acquire);

        loop {
            if observed & STATE_MASK == STATE_IDLE {
                return;
            }
            match state.compare_exchange_weak(
                observed,
                next_idle_epoch(observed),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => observed = actual,
            }
        }
    }
}

const fn next_idle_epoch(state: usize) -> usize {
    (state & !STATE_MASK).wrapping_add(EPOCH_STEP)
}

impl Default for DeliveryEdge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub(crate) struct DeliveryEdges<const CPU_COUNT: usize> {
    edges: [DeliveryEdge; CPU_COUNT],
}

#[cfg(test)]
impl<const CPU_COUNT: usize> DeliveryEdges<CPU_COUNT> {
    pub(crate) const fn new() -> Self {
        Self {
            edges: [const { DeliveryEdge::new() }; CPU_COUNT],
        }
    }

    pub(crate) fn notify(
        &self,
        cpu: CpuId,
        send: impl FnOnce() -> Result<(), IrqError>,
    ) -> Result<IpiNotification, IrqError> {
        self.edges
            .get(cpu.0)
            .ok_or(IrqError::InvalidCpu)?
            .notify(send)
    }

    pub(crate) fn claim(&self, cpu: CpuId) {
        if let Some(edge) = self.edges.get(cpu.0) {
            edge.claim();
        }
    }
}
