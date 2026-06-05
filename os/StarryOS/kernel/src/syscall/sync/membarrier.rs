use core::sync::atomic::{Ordering, fence};

use ax_errno::{AxError, AxResult};
use ax_task::current;

use crate::task::AsThread;

/// Memory barrier commands
const MEMBARRIER_CMD_QUERY: i32 = 0;
const MEMBARRIER_CMD_GLOBAL: i32 = 1;
const MEMBARRIER_CMD_GLOBAL_EXPEDITED: i32 = 2;
const MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED: i32 = 3;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 = 4;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 = 5;

const MEMBARRIER_STATE_PRIVATE_EXPEDITED: u32 = 1 << 0;
const MEMBARRIER_STATE_GLOBAL_EXPEDITED: u32 = 1 << 1;

/// Supported command flags for query
const SUPPORTED_COMMANDS: i32 = (1 << MEMBARRIER_CMD_GLOBAL)
    | (1 << MEMBARRIER_CMD_GLOBAL_EXPEDITED)
    | (1 << MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED)
    | (1 << MEMBARRIER_CMD_PRIVATE_EXPEDITED)
    | (1 << MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED);

fn smp_mb() {
    fence(Ordering::SeqCst);
}

pub fn sys_membarrier(cmd: i32, flags: u32, _cpu_id: i32) -> AxResult<isize> {
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    match cmd {
        MEMBARRIER_CMD_QUERY => Ok(SUPPORTED_COMMANDS as isize),
        MEMBARRIER_CMD_GLOBAL => {
            smp_mb();
            Ok(0)
        }
        MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED => {
            current()
                .as_thread()
                .proc_data
                .register_membarrier_state(MEMBARRIER_STATE_GLOBAL_EXPEDITED);
            Ok(0)
        }
        MEMBARRIER_CMD_GLOBAL_EXPEDITED => {
            smp_mb();
            Ok(0)
        }
        MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED => {
            current()
                .as_thread()
                .proc_data
                .register_membarrier_state(MEMBARRIER_STATE_PRIVATE_EXPEDITED);
            Ok(0)
        }
        MEMBARRIER_CMD_PRIVATE_EXPEDITED => {
            let proc_data = current().as_thread().proc_data.clone();
            if proc_data.membarrier_state() & MEMBARRIER_STATE_PRIVATE_EXPEDITED == 0 {
                return Err(AxError::OperationNotPermitted);
            }
            smp_mb();
            Ok(0)
        }
        _ => Err(AxError::InvalidInput),
    }
}
