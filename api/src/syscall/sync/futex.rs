use core::sync::atomic::Ordering;

use axerrno::{LinuxError, LinuxResult};
use axtask::current;
use linux_raw_sys::general::{
    FUTEX_CMD_MASK, FUTEX_CMP_REQUEUE, FUTEX_REQUEUE, FUTEX_WAIT, FUTEX_WAIT_BITSET, FUTEX_WAKE,
    FUTEX_WAKE_BITSET, robust_list_head, timespec,
};
use starry_core::{
    futex::FutexKey,
    task::{AsThread, get_task},
};

use crate::{
    mm::{UserConstPtr, UserPtr, nullable},
    time::TimeValueLike,
};

fn assert_unsigned(value: u32) -> LinuxResult<u32> {
    if (value as i32) < 0 {
        Err(LinuxError::EINVAL)
    } else {
        Ok(value)
    }
}

pub fn sys_futex(
    uaddr: UserConstPtr<u32>,
    futex_op: u32,
    value: u32,
    timeout: UserConstPtr<timespec>,
    uaddr2: UserPtr<u32>,
    value3: u32,
) -> LinuxResult<isize> {
    debug!(
        "sys_futex <= uaddr: {:?}, futex_op: {}, value: {}, uaddr2: {:?}, value3: {}",
        uaddr.address(),
        futex_op,
        value,
        uaddr2.address(),
        value3,
    );

    let key = FutexKey::new_current(uaddr.address().as_usize());

    let curr = current();
    let thr = curr.as_thread();
    let proc_data = &thr.proc_data;
    let futex_table = proc_data.futex_table_for(&key);

    let command = futex_op & (FUTEX_CMD_MASK as u32);
    match command {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let uaddr_ref = uaddr.get_as_ref()?;

            // Fast path
            if *uaddr_ref != value {
                return Err(LinuxError::EAGAIN);
            }

            let timeout = nullable!(timeout.get_as_ref())?
                .map(|ts| ts.try_into_time_value())
                .transpose()?;

            // This function is called with the lock to run queue being held by
            // us, and thus we need to check FOR ONCE if the value has changed.
            // If so, we shall skip waiting and return EAGAIN; otherwise, we
            // return false to start waiting and return true for subsequent
            // calls.
            let mut first_call = true;
            let mut mismatches = false;
            let condition = || {
                if first_call {
                    mismatches = *uaddr_ref != value;
                    first_call = false;
                    mismatches
                } else {
                    true
                }
            };

            let futex = futex_table.get_or_insert(&key);

            if command == FUTEX_WAIT_BITSET {
                thr.set_futex_bitset(value3);
            }

            if let Some(timeout) = timeout {
                if futex.wq.wait_timeout_until(timeout, condition) {
                    return Err(LinuxError::ETIMEDOUT);
                }
            } else {
                futex.wq.wait_until(condition);
            }
            if mismatches {
                return Err(LinuxError::EAGAIN);
            }

            if futex.owner_dead.swap(false, Ordering::SeqCst) {
                Err(LinuxError::EOWNERDEAD)
            } else {
                Ok(0)
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let futex = futex_table.get(&key);
            let mut count = 0;
            if let Some(futex) = futex {
                futex.wq.notify_all_if(false, |task| {
                    if count >= value {
                        false
                    } else {
                        let wake = if command == FUTEX_WAKE_BITSET {
                            let bitset = task.as_thread().futex_bitset();
                            (bitset & value3) != 0
                        } else {
                            true
                        };
                        count += wake as u32;
                        wake
                    }
                });
            }
            axtask::yield_now();
            Ok(count as isize)
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            assert_unsigned(value)?;
            if command == FUTEX_CMP_REQUEUE && *uaddr.get_as_ref()? != value3 {
                return Err(LinuxError::EAGAIN);
            }
            let value2 = assert_unsigned(timeout.address().as_usize() as u32)?;

            let futex = futex_table.get(&key);
            let key2 = FutexKey::new_current(uaddr2.address().as_usize());
            let table2 = proc_data.futex_table_for(&key2);
            let futex2 = table2.get_or_insert(&key2);

            let mut count = 0;
            if let Some(futex) = futex {
                for _ in 0..value {
                    if !futex.wq.notify_one(false) {
                        break;
                    }
                    count += 1;
                }
                if count == value as isize {
                    count += futex.wq.requeue(value2 as usize, &futex2.wq) as isize;
                }
            }
            Ok(count)
        }
        _ => Err(LinuxError::ENOSYS),
    }
}

pub fn sys_get_robust_list(
    tid: u32,
    head: UserPtr<UserConstPtr<robust_list_head>>,
    size: UserPtr<usize>,
) -> LinuxResult<isize> {
    let task = get_task(tid)?;
    *head.get_as_mut()? = task.as_thread().robust_list_head().into();
    *size.get_as_mut()? = size_of::<robust_list_head>();

    Ok(0)
}

pub fn sys_set_robust_list(
    head: UserConstPtr<robust_list_head>,
    size: usize,
) -> LinuxResult<isize> {
    if size != size_of::<robust_list_head>() {
        return Err(LinuxError::EINVAL);
    }
    current()
        .as_thread()
        .set_robust_list_head(head.address().as_usize());

    Ok(0)
}
