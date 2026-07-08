//! Lightweight socket state gate.
//!
//! Socket methods use this atomic guard to serialize high-level state
//! transitions without holding the global smoltcp socket-set lock across an
//! entire POSIX operation. It is intentionally smaller than a mutex: one method
//! can move a socket into `Busy`, perform the protocol operation, then commit or
//! roll back the public state.
//!
//! # Relationship To smoltcp State
//!
//! This state is the user-visible control state, not a replacement for
//! smoltcp's TCP state machine. For example, a TCP socket may be publicly
//! `Connecting` while the smoltcp socket is `SynSent`. Socket code must update
//! both sides at clear transition points.

use core::sync::atomic::{AtomicU8, Ordering};

use ax_errno::AxResult;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Busy,
    Connecting,
    Connected,
    Listening,
    Closed,
}

impl TryFrom<u8> for State {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, ()> {
        Ok(match value {
            0 => State::Idle,
            1 => State::Busy,
            2 => State::Connecting,
            3 => State::Connected,
            4 => State::Listening,
            5 => State::Closed,
            _ => return Err(()),
        })
    }
}

pub struct StateLock(AtomicU8);
impl StateLock {
    /// Creates a state gate initialized to `state`.
    pub fn new(state: State) -> Self {
        Self(AtomicU8::new(state as u8))
    }

    /// Loads the current public socket state.
    pub fn get(&self) -> State {
        self.0
            .load(Ordering::Acquire)
            .try_into()
            .expect("invalid state")
    }

    /// Stores a new public socket state.
    pub fn set(&self, state: State) {
        self.0.store(state as u8, Ordering::Release);
    }

    /// Moves from `expect` to `Busy`, returning the observed state on failure.
    pub fn lock(&self, expect: State) -> Result<StateGuard<'_>, State> {
        match self.0.compare_exchange(
            expect as u8,
            State::Busy as u8,
            Ordering::Acquire,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(StateGuard(self, expect as u8)),
            Err(old) => Err(old.try_into().expect("invalid state")),
        }
    }
}

#[must_use]
/// Guard for a pending state transition.
pub struct StateGuard<'a>(&'a StateLock, u8);
impl StateGuard<'_> {
    /// Runs a transition body and commits the new state only on success.
    pub fn transit<R>(self, new: State, f: impl FnOnce() -> AxResult<R>) -> AxResult<R> {
        match f() {
            Ok(result) => {
                self.0.0.store(new as u8, Ordering::Release);
                Ok(result)
            }
            Err(err) => {
                self.0.0.store(self.1, Ordering::Release);
                Err(err)
            }
        }
    }
}
