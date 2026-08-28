use core::sync::atomic::{Ordering, fence};

use ax_task::current;
use linux_raw_sys::general::membarrier_cmd;

use crate::{StarryError, StarryResult, task::AsThread};

/// Memory barrier commands
const MEMBARRIER_CMD_QUERY: i32 = membarrier_cmd::MEMBARRIER_CMD_QUERY as i32;
const MEMBARRIER_CMD_GLOBAL: i32 = membarrier_cmd::MEMBARRIER_CMD_GLOBAL as i32;
const MEMBARRIER_CMD_GLOBAL_EXPEDITED: i32 = membarrier_cmd::MEMBARRIER_CMD_GLOBAL_EXPEDITED as i32;
const MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED: i32 =
    membarrier_cmd::MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED as i32;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i32 =
    membarrier_cmd::MEMBARRIER_CMD_PRIVATE_EXPEDITED as i32;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i32 =
    membarrier_cmd::MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED as i32;

const MEMBARRIER_STATE_PRIVATE_EXPEDITED: u32 = MEMBARRIER_CMD_PRIVATE_EXPEDITED as u32;
const MEMBARRIER_STATE_GLOBAL_EXPEDITED: u32 = MEMBARRIER_CMD_GLOBAL_EXPEDITED as u32;

/// Supported command flags for query
const SUPPORTED_COMMANDS: i32 = MEMBARRIER_CMD_GLOBAL
    | MEMBARRIER_CMD_GLOBAL_EXPEDITED
    | MEMBARRIER_CMD_REGISTER_GLOBAL_EXPEDITED
    | MEMBARRIER_CMD_PRIVATE_EXPEDITED
    | MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED;

fn smp_mb() {
    fence(Ordering::SeqCst);
}

pub fn sys_membarrier(cmd: i32, flags: u32, _cpu_id: i32) -> StarryResult<isize> {
    if flags != 0 {
        return Err(StarryError::InvalidInput);
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
            let proc_data = current().as_thread().proc_data.clone();
            if proc_data.membarrier_state() & MEMBARRIER_STATE_GLOBAL_EXPEDITED == 0 {
                return Err(StarryError::OperationNotPermitted);
            }
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
                return Err(StarryError::OperationNotPermitted);
            }
            smp_mb();
            Ok(0)
        }
        _ => Err(StarryError::InvalidInput),
    }
}

#[cfg(all(test, not(axtest)))]
fn membarrier_query_and_global_rules_hold_for_test() -> bool {
    matches!(
        sys_membarrier(MEMBARRIER_CMD_QUERY, 0, 0),
        Ok(value) if value == SUPPORTED_COMMANDS as isize
    ) && matches!(
        sys_membarrier(MEMBARRIER_CMD_QUERY, 1, 0),
        Err(StarryError::InvalidInput)
    ) && matches!(sys_membarrier(-1, 0, 0), Err(StarryError::InvalidInput))
        && matches!(sys_membarrier(MEMBARRIER_CMD_GLOBAL, 0, 0), Ok(0))
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn membarrier_query_and_global_rules_hold() {
        assert!(super::membarrier_query_and_global_rules_hold_for_test());
    }
}
