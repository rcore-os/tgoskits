use starry_vm::VmError;

/// Errors produced by signal-management operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignalError {
    /// A signal operation could not access the caller's userspace memory.
    #[error("signal user-memory access failed: {0}")]
    UserMemory(
        #[from]
        #[source]
        VmError,
    ),
}

/// A signal-management result.
pub type SignalResult<T> = Result<T, SignalError>;

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use core::error::Error as _;

    use super::*;

    #[test]
    fn user_memory_error_preserves_source() {
        let err = SignalError::from(VmError::BadAddress);
        assert_eq!(
            err.to_string(),
            "signal user-memory access failed: virtual address is invalid"
        );
        assert_eq!(
            err.source().unwrap().to_string(),
            "virtual address is invalid"
        );
    }
}
