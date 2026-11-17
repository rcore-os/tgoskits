//! Component observations distinguish publication conflicts from backend errors.

use crate::{VfsError, VfsResult};

/// Why an unaccepted component must be observed again.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LookupConflict {
    /// The local cache or provider version changed during candidate acquisition.
    CandidatePublication,
    /// A namespace writer overlapped, or a parent naming observation expired.
    ParentNaming,
}

/// One component attempt. Missing is accepted only after publication checks;
/// backend WouldBlock remains a real failure, never an inferred restart request.
#[must_use]
#[derive(Debug)]
pub enum LookupStep<T> {
    Found(T),
    Missing,
    Revalidate(LookupConflict),
    Failed(VfsError),
}

impl<T> LookupStep<T> {
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> LookupStep<U> {
        match self {
            Self::Found(value) => LookupStep::Found(map(value)),
            Self::Missing => LookupStep::Missing,
            Self::Revalidate(reason) => LookupStep::Revalidate(reason),
            Self::Failed(error) => LookupStep::Failed(error),
        }
    }

    // Fixed one-attempt callers do not own a user lookup session. They expose
    // publication contention as an error, without retrying or guessing its cause.
    pub(crate) fn into_result(self) -> VfsResult<T> {
        match self {
            Self::Found(value) => Ok(value),
            Self::Missing => Err(VfsError::NotFound),
            Self::Revalidate(_) => Err(VfsError::WouldBlock),
            Self::Failed(error) => Err(error),
        }
    }
}
