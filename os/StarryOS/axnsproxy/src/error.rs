/// Errors owned by PID namespace and identity operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PidError {
    /// Memory or a bounded PID ownership counter could not be allocated.
    #[error("PID namespace allocation failed")]
    AllocationFailed,
    /// The requested PID identity already exists in this namespace generation.
    #[error("PID identity already exists")]
    AlreadyExists,
    /// The supplied namespace or reservation parameters are invalid.
    #[error("invalid PID namespace input")]
    InvalidInput,
    /// PID indexes or publication state violate their ownership invariant.
    #[error("PID namespace state is inconsistent")]
    InvalidState,
    /// The requested process identity is not live and published.
    #[error("PID namespace process does not exist")]
    NoSuchProcess,
    /// The namespace lifecycle no longer accepts PID publication.
    #[error("PID namespace is unavailable for publication")]
    NamespaceUnavailable,
}

/// A result returned by PID namespace and identity operations.
pub type PidResult<T = ()> = Result<T, PidError>;
