use core::sync::atomic::{self, Ordering};

use ax_errno::{AxError, AxResult};

/// Memory barrier commands (values match Linux uapi/linux/membarrier.h).
const MEMBARRIER_CMD_QUERY: i32 = 0;
const _MEMBARRIER_CMD_GLOBAL: i32 = 1 << 0;
const _MEMBARRIER_CMD_GLOBAL_EXPEDITED: i32 = 1 << 1;
const MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED: i32 = 1 << 2;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 = 1 << 3;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 = 1 << 4;

/// Bitmask of commands we actually support.
///
/// We only advertise private-expedited commands because we cannot issue
/// IPIs to other CPUs; a local atomic fence is the best we can do.
const SUPPORTED_COMMANDS: i32 =
    MEMBARRIER_CMD_PRIVATE_EXPEDITED
    | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED;

pub fn sys_membarrier(cmd: i32, flags: u32, _cpu_id: i32) -> AxResult<isize> {
    match cmd {
        MEMBARRIER_CMD_QUERY => {
            if flags != 0 {
                return Err(AxError::InvalidInput);
            }
            Ok(SUPPORTED_COMMANDS as isize)
        }
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED
        | MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED
        | MEMBARRIER_CMD_PRIVATE_EXPEDITED
        | _MEMBARRIER_CMD_GLOBAL
        | _MEMBARRIER_CMD_GLOBAL_EXPEDITED => {
            if flags != 0 {
                return Err(AxError::InvalidInput);
            }
            // Issue a full memory barrier. This is not a true cross-CPU
            // synchronisation (we lack IPI support), but provides
            // single-CPU ordering that is sufficient for cooperative
            // scheduling and unikernel workloads.
            atomic::fence(Ordering::SeqCst);
            Ok(0)
        }
        _ => {
            // Unknown command.
            Err(AxError::InvalidInput)
        }
    }
}
