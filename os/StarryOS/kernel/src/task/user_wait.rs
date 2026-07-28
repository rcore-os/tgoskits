use core::task::Poll;

/// Terminal outcome of one Starry user-task wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserWaitOutcome<T> {
    /// The waited operation completed.
    Ready(T),
    /// A deliverable signal interrupted the wait.
    Interrupted,
    /// The wait deadline elapsed.
    TimedOut,
}

impl<T> UserWaitOutcome<T> {
    /// Converts the typed terminal state into a conventional result.
    pub fn into_result(self) -> Result<T, UserWaitError> {
        match self {
            Self::Ready(output) => Ok(output),
            Self::Interrupted => Err(UserWaitError::Interrupted),
            Self::TimedOut => Err(UserWaitError::TimedOut),
        }
    }
}

/// Reason a Starry user-task wait did not complete its operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserWaitError {
    /// A deliverable signal interrupted the wait.
    Interrupted,
    /// The wait deadline elapsed.
    TimedOut,
}

impl core::fmt::Display for UserWaitError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Interrupted => formatter.write_str("interrupted"),
            Self::TimedOut => formatter.write_str("deadline elapsed"),
        }
    }
}

impl core::error::Error for UserWaitError {}

pub(crate) fn resolve_user_wait<T>(
    future: Poll<T>,
    interrupted: bool,
    timed_out: bool,
) -> Poll<UserWaitOutcome<T>> {
    if let Poll::Ready(output) = future {
        return Poll::Ready(UserWaitOutcome::Ready(output));
    }
    if interrupted {
        return Poll::Ready(UserWaitOutcome::Interrupted);
    }
    if timed_out {
        return Poll::Ready(UserWaitOutcome::TimedOut);
    }
    Poll::Pending
}
