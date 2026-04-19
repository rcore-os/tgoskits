use ax_errno::AxError;
use ax_task::current;
use starry_vm::VmPtr;

use crate::task::AsThread;

fn validate_rseq_addr(addr: *mut u8, len: usize) -> Result<Option<usize>, AxError> {
    if addr.is_null() {
        if len != 0 {
            return Err(AxError::InvalidInput);
        }
        return Ok(None);
    }

    if len == 0 {
        return Err(AxError::InvalidInput);
    }

    Ok(Some(addr.addr()))
}

/// Minimal implementation of the rseq syscall registration.
///
/// This implementation only supports registration/unregistration via the
/// first argument (addr) and the flags argument. It stores the user pointer
/// in the current thread structure so kernel-side users can inspect it.
///
/// C prototype (simplified):
/// long rseq(void *addr, uint32_t len, int flags, uint32_t sig);
pub fn sys_rseq(addr: *mut u8, len: usize, flags: u32, sig: u32) -> Result<isize, AxError> {
    debug!(
        "sys_rseq <= addr: {:?}, len: {}, flags: {}, sig: {}",
        addr, len, flags, sig
    );

    let Some(addr) = validate_rseq_addr(addr, len)? else {
        current().as_thread().set_rseq_area(0);
        return Ok(0);
    };

    // Check that the user pointer is readable/writable (we only need the address).
    // Try to read one byte to ensure the area is valid.
    if (addr as *mut u8).vm_read().is_err() {
        return Err(AxError::InvalidInput);
    }

    // Store the user address in the thread.
    current().as_thread().set_rseq_area(addr);

    Ok(0)
}

#[cfg(test)]
mod tests {
    use ax_errno::AxError;

    use super::validate_rseq_addr;

    #[test]
    fn validate_rseq_addr_allows_unregister() {
        assert_eq!(validate_rseq_addr(core::ptr::null_mut(), 0).unwrap(), None);
    }

    #[test]
    fn validate_rseq_addr_rejects_null_addr_with_nonzero_len() {
        assert_eq!(
            validate_rseq_addr(core::ptr::null_mut(), 8).unwrap_err(),
            AxError::InvalidInput
        );
    }

    #[test]
    fn validate_rseq_addr_rejects_nonnull_addr_with_zero_len() {
        assert_eq!(
            validate_rseq_addr(1usize as *mut u8, 0).unwrap_err(),
            AxError::InvalidInput
        );
    }
}
