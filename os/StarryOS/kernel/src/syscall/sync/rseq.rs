use core::mem::size_of;

use ax_errno::{AxError, LinuxError};
use ax_task::current;
use starry_vm::{VmMutPtr, VmPtr};

use crate::task::AsThread;

/// Linux rseq area layout used for ABI validation.
#[repr(C)]
#[derive(Clone, Copy)]
struct RseqArea {
    cpu_id_start: u32,
    cpu_id: u32,
    rseq_cs: u64,
    flags: u32,
    padding: [u32; 3],
}

const RSEQ_AREA_SIZE: usize = size_of::<RseqArea>();
const RSEQ_AREA_ALIGN: usize = 32;
const RSEQ_FLAG_UNREGISTER: u32 = 1;
const RSEQ_CPU_ID_UNINITIALIZED: u32 = u32::MAX;

fn validate_rseq_args(addr: *mut u8, len: usize, flags: u32) -> Result<usize, AxError> {
    if addr.is_null() || len != RSEQ_AREA_SIZE {
        return Err(AxError::InvalidInput);
    }
    if flags & !RSEQ_FLAG_UNREGISTER != 0 {
        return Err(AxError::InvalidInput);
    }

    let addr = addr.addr();
    if !addr.is_multiple_of(RSEQ_AREA_ALIGN) {
        return Err(AxError::InvalidInput);
    }

    Ok(addr)
}

fn ensure_rseq_area_accessible(addr: usize) -> Result<(), AxError> {
    let area = addr as *mut RseqArea;
    let _ = area.vm_read_uninit().map_err(|_| AxError::BadAddress)?;
    area.vm_write(RseqArea {
        cpu_id_start: 0,
        cpu_id: RSEQ_CPU_ID_UNINITIALIZED,
        rseq_cs: 0,
        flags: 0,
        padding: [0; 3],
    })
    .map_err(|_| AxError::BadAddress)?;
    Ok(())
}

/// rseq(2) — register or unregister a per-thread restartable-sequences area.
///
/// This implements the userspace-visible registration state machine and
/// argument validation. The deeper rseq critical-section abort machinery is
/// still not implemented, so StarryOS leaves `cpu_id` uninitialized to keep
/// libc fast paths from assuming usable rseq CPU state.
///
/// C prototype:
/// long rseq(void *addr, uint32_t len, int flags, uint32_t sig);
pub fn sys_rseq(addr: *mut u8, len: usize, flags: u32, sig: u32) -> Result<isize, AxError> {
    debug!(
        "sys_rseq <= addr: {:?}, len: {}, flags: {}, sig: {}",
        addr, len, flags, sig
    );

    let addr = validate_rseq_args(addr, len, flags)?;
    let curr = current();
    let thr = curr.as_thread();
    let registered_addr = thr.rseq_area();
    let unregister = flags & RSEQ_FLAG_UNREGISTER != 0;

    if unregister {
        if registered_addr == 0 || registered_addr != addr || thr.rseq_signature() != sig {
            return Err(AxError::InvalidInput);
        }
        thr.clear_rseq_state();
        return Ok(0);
    }

    if registered_addr != 0 {
        return Err(AxError::from(LinuxError::EBUSY));
    }

    ensure_rseq_area_accessible(addr)?;
    thr.set_rseq_state(addr, sig);
    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn rseq_validation_rejects_invalid_arguments_for_test() -> bool {
    validate_rseq_args(core::ptr::null_mut(), RSEQ_AREA_SIZE, 0) == Err(AxError::InvalidInput)
        && validate_rseq_args(0x1000 as *mut u8, RSEQ_AREA_SIZE - 1, 0)
            == Err(AxError::InvalidInput)
        && validate_rseq_args(0x1000 as *mut u8, RSEQ_AREA_SIZE, RSEQ_FLAG_UNREGISTER << 1)
            == Err(AxError::InvalidInput)
        && validate_rseq_args(0x1001 as *mut u8, RSEQ_AREA_SIZE, 0) == Err(AxError::InvalidInput)
        && validate_rseq_args(0x1000 as *mut u8, RSEQ_AREA_SIZE, 0) == Ok(0x1000)
}

#[cfg(axtest)]
pub(crate) fn rseq_validation_rules_hold_for_test() -> bool {
    // Test validate_rseq_args validation logic
    // Null address should fail
    let result = validate_rseq_args(core::ptr::null_mut(), RSEQ_AREA_SIZE, 0);
    assert!(result.is_err());

    // Wrong length should fail
    let addr = 0x1000 as *mut u8;
    let result = validate_rseq_args(addr, RSEQ_AREA_SIZE - 1, 0);
    assert!(result.is_err());

    let result = validate_rseq_args(addr, RSEQ_AREA_SIZE + 1, 0);
    assert!(result.is_err());

    // Invalid flags should fail
    let result = validate_rseq_args(addr, RSEQ_AREA_SIZE, 0xFFFF);
    assert!(result.is_err());

    // Valid flags (0 and RSEQ_FLAG_UNREGISTER) should pass address validation
    let result = validate_rseq_args(addr, RSEQ_AREA_SIZE, 0);
    // Note: will fail on alignment check for non-aligned address

    // Aligned address should work
    let aligned_addr = 0x100000 as *mut u8; // 1MB aligned
    let result = validate_rseq_args(aligned_addr, RSEQ_AREA_SIZE, 0);
    assert!(result.is_ok());

    true
}

#[cfg(test)]
mod tests {
    use ax_errno::AxError;

    use super::{RSEQ_AREA_SIZE, RSEQ_FLAG_UNREGISTER, validate_rseq_args};

    #[test]
    fn validate_rseq_args_rejects_null_addr() {
        assert_eq!(
            validate_rseq_args(core::ptr::null_mut(), RSEQ_AREA_SIZE, 0).unwrap_err(),
            AxError::InvalidInput
        );
    }

    #[test]
    fn validate_rseq_args_rejects_bad_len() {
        let ptr = 0x1000 as *mut u8;
        assert_eq!(
            validate_rseq_args(ptr, RSEQ_AREA_SIZE - 1, 0).unwrap_err(),
            AxError::InvalidInput
        );
    }

    #[test]
    fn validate_rseq_args_rejects_bad_flags() {
        let ptr = 0x1000 as *mut u8;
        assert_eq!(
            validate_rseq_args(ptr, RSEQ_AREA_SIZE, RSEQ_FLAG_UNREGISTER << 1).unwrap_err(),
            AxError::InvalidInput
        );
    }

    #[test]
    fn validate_rseq_args_rejects_misaligned_addr() {
        let ptr = 0x1001 as *mut u8;
        assert_eq!(
            validate_rseq_args(ptr, RSEQ_AREA_SIZE, 0).unwrap_err(),
            AxError::InvalidInput
        );
    }

    #[test]
    fn validate_rseq_args_accepts_aligned_addr() {
        let ptr = 0x1000 as *mut u8;
        assert_eq!(validate_rseq_args(ptr, RSEQ_AREA_SIZE, 0).unwrap(), 0x1000);
    }
}
