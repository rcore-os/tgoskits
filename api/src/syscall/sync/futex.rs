use core::sync::atomic::Ordering;

use axerrno::{AxError, AxResult, LinuxError};
use axtask::current;
use linux_raw_sys::general::{
    FUTEX_CMD_MASK, FUTEX_CMP_REQUEUE, FUTEX_REQUEUE, FUTEX_WAIT, FUTEX_WAIT_BITSET, FUTEX_WAKE,
    FUTEX_WAKE_BITSET, robust_list_head, timespec,
};
use starry_core::{
    futex::FutexKey,
    task::{AsThread, get_task},
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::time::TimeValueLike;

fn assert_unsigned(value: u32) -> AxResult<u32> {
    if (value as i32) < 0 {
        Err(AxError::InvalidInput)
    } else {
        Ok(value)
    }
}

pub fn sys_futex(
    uaddr: *const u32,
    futex_op: u32,
    value: u32,
    timeout: *const timespec,
    uaddr2: *mut u32,
    value3: u32,
) -> AxResult<isize> {
    debug!(
        "sys_futex <= uaddr: {uaddr:?}, futex_op: {futex_op}, value: {value}, uaddr2: {uaddr2:?}, \
         value3: {value3}",
    );

    let key = FutexKey::new_current(uaddr.addr());

    let curr = current();
    let thr = curr.as_thread();
    let proc_data = &thr.proc_data;
    let futex_table = proc_data.futex_table_for(&key);

    let command = futex_op & (FUTEX_CMD_MASK as u32);
    match command {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            // Fast path
            if uaddr.vm_read()? != value {
                return Err(AxError::WouldBlock);
            }

            let timeout = if let Some(ts) = timeout.nullable() {
                // FIXME: AnyBitPattern
                let ts = unsafe { ts.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
                Some(ts)
            } else {
                None
            };

            let futex = futex_table.get_or_insert(&key);

            let bitset = if command == FUTEX_WAIT_BITSET {
                value3
            } else {
                u32::MAX
            };

            if !futex
                .wq
                .wait_if(bitset, timeout, || uaddr.vm_read() == Ok(value))?
            {
                return Err(AxError::WouldBlock);
            }

            if futex.owner_dead.swap(false, Ordering::SeqCst) {
                Err(AxError::Other(LinuxError::EOWNERDEAD))
            } else {
                Ok(0)
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let futex = futex_table.get(&key);
            let mut count = 0;
            if let Some(futex) = futex {
                let bitset = if command == FUTEX_WAKE_BITSET {
                    value3
                } else {
                    u32::MAX
                };
                count = futex.wq.wake(value as _, bitset);
            }
            axtask::yield_now();
            Ok(count as _)
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            assert_unsigned(value)?;
            if command == FUTEX_CMP_REQUEUE && uaddr.vm_read()? != value3 {
                return Err(AxError::WouldBlock);
            }
            let value2 = assert_unsigned(timeout.addr() as u32)?;

            let futex = futex_table.get(&key);
            let key2 = FutexKey::new_current(uaddr2.addr());
            let table2 = proc_data.futex_table_for(&key2);
            let futex2 = table2.get_or_insert(&key2);

            let mut count = 0;
            if let Some(futex) = futex {
                count = futex.wq.wake(value as _, u32::MAX);
                if count == value as usize {
                    count += futex.wq.requeue(value2 as _, &futex2.wq) as usize;
                }
            }
            Ok(count as _)
        }
        _ => Err(AxError::Unsupported),
    }
}

pub fn sys_get_robust_list(
    tid: u32,
    head: *mut *const robust_list_head,
    size: *mut usize,
) -> AxResult<isize> {
    let task = get_task(tid)?;
    head.vm_write(task.as_thread().robust_list_head() as _)?;
    size.vm_write(size_of::<robust_list_head>())?;

    Ok(0)
}

pub fn sys_set_robust_list(head: *const robust_list_head, size: usize) -> AxResult<isize> {
    if size != size_of::<robust_list_head>() {
        return Err(AxError::InvalidInput);
    }
    current().as_thread().set_robust_list_head(head.addr());

    Ok(0)
}
