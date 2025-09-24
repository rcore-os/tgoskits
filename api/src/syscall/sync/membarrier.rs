use core::sync::atomic::{Ordering, compiler_fence};

use axerrno::{AxError, AxResult};

/// Memory barrier commands
const MEMBARRIER_CMD_QUERY: i32 = 0;
const MEMBARRIER_CMD_GLOBAL: i32 = 1;
const MEMBARRIER_CMD_GLOBAL_EXPEDITED: i32 = 2;
const MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED: i32 = 3;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 = 4;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 = 5;

/// Supported command flags for query
const SUPPORTED_COMMANDS: i32 = (1 << MEMBARRIER_CMD_GLOBAL)
    | (1 << MEMBARRIER_CMD_GLOBAL_EXPEDITED)
    | (1 << MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED)
    | (1 << MEMBARRIER_CMD_PRIVATE_EXPEDITED)
    | (1 << MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED);

pub fn sys_membarrier(cmd: i32, flags: u32, _cpu_id: i32) -> AxResult<isize> {
    // 检查 flags 参数，目前应该为 0
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    match cmd {
        MEMBARRIER_CMD_QUERY => Ok(SUPPORTED_COMMANDS as isize),
        _ => {
            compiler_fence(Ordering::SeqCst);
            Ok(0)
        }
    }
}
