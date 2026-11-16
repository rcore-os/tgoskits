//! A snapshot of a registered read, preserving typed restart control.

use crate::{NamespaceOutcome, NamespaceState, VfsError, VfsResult};

/// Read values never authorize a mutation. Retry is backend control, not errno.
#[derive(Clone)]
pub enum NamespaceReadResult<T> {
    Ready(VfsResult<T>),
    Retry,
}

impl<T> NamespaceReadResult<T> {
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> NamespaceReadResult<U> {
        match self {
            Self::Ready(value) => NamespaceReadResult::Ready(value.map(map)),
            Self::Retry => NamespaceReadResult::Retry,
        }
    }

    pub fn and_then<U>(self, map: impl FnOnce(T) -> VfsResult<U>) -> NamespaceReadResult<U> {
        match self {
            Self::Ready(value) => NamespaceReadResult::Ready(value.and_then(map)),
            Self::Retry => NamespaceReadResult::Retry,
        }
    }

    pub(crate) fn capture(outcome: &NamespaceOutcome<T>) -> Self
    where
        T: Clone,
    {
        match outcome.state() {
            NamespaceState::Retry => Self::Retry,
            NamespaceState::Unchanged => Self::Ready(
                outcome
                    .result()
                    .and_then(|()| outcome.observation().cloned().ok_or(VfsError::BadState)),
            ),
            NamespaceState::Applied | NamespaceState::Uncertain => {
                Self::Ready(outcome.result().and(Err(VfsError::BadState)))
            }
        }
    }
}
