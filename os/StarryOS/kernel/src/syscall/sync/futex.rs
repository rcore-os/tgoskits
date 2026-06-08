use core::mem::align_of;

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::{TimeValue, monotonic_time, wall_time};
use ax_task::current;
use linux_raw_sys::general::{
    FUTEX_CLOCK_REALTIME, FUTEX_CMP_REQUEUE, FUTEX_OP_ADD, FUTEX_OP_ANDN, FUTEX_OP_CMP_EQ,
    FUTEX_OP_CMP_GE, FUTEX_OP_CMP_GT, FUTEX_OP_CMP_LE, FUTEX_OP_CMP_LT, FUTEX_OP_CMP_NE,
    FUTEX_OP_OPARG_SHIFT, FUTEX_OP_OR, FUTEX_OP_SET, FUTEX_OP_XOR, FUTEX_REQUEUE, FUTEX_WAIT,
    FUTEX_WAIT_BITSET, FUTEX_WAKE, FUTEX_WAKE_BITSET, FUTEX_WAKE_OP, robust_list_head, timespec,
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    mm::atomic_update_user_u32,
    task::{AsThread, FutexKey, FutexKeyMode, futex_table_for, get_task},
    time::TimeValueLike,
};

const FUTEX_PRIVATE_FLAG: u32 = 128;
const FUTEX_COMMAND_MASK: u32 = FUTEX_PRIVATE_FLAG - 1;
const SUPPORTED_FLAGS: u32 = FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FutexCommand {
    Wait,
    Wake,
    WaitBitset,
    WakeBitset,
    Requeue,
    CmpRequeue,
    WakeOp,
}

struct ParsedFutexOp {
    command: FutexCommand,
    key_mode: FutexKeyMode,
    clock_realtime: bool,
}

fn assert_non_negative_i32(value: u32) -> AxResult<u32> {
    if (value as i32) < 0 {
        Err(AxError::InvalidInput)
    } else {
        Ok(value)
    }
}

fn validate_futex_word(uaddr: *const u32) -> AxResult<()> {
    if !uaddr.addr().is_multiple_of(align_of::<u32>()) {
        return Err(AxError::InvalidInput);
    }
    uaddr.vm_read()?;
    Ok(())
}

fn sign_extend_12(value: u32) -> i32 {
    ((value << 20) as i32) >> 20
}

fn futex_wake_op_arg(raw_op: u32, encoded_op: u32) -> i32 {
    let mut oparg = sign_extend_12((encoded_op >> 12) & 0xfff);
    if raw_op & FUTEX_OP_OPARG_SHIFT != 0 {
        oparg = (1u32 << ((oparg & 31) as u32)) as i32;
    }
    oparg
}

fn apply_futex_wake_op(old_value: u32, raw_op: u32, oparg: i32) -> AxResult<u32> {
    let op = raw_op & !FUTEX_OP_OPARG_SHIFT;
    let new_value = match op {
        FUTEX_OP_SET => oparg as u32,
        FUTEX_OP_ADD => (old_value as i32).wrapping_add(oparg) as u32,
        FUTEX_OP_OR => old_value | oparg as u32,
        FUTEX_OP_ANDN => old_value & !(oparg as u32),
        FUTEX_OP_XOR => old_value ^ oparg as u32,
        _ => return Err(AxError::Unsupported),
    };
    Ok(new_value)
}

fn compare_futex_wake_op(old_value: u32, raw_cmp: u32, cmparg: i32) -> AxResult<bool> {
    let old_value = old_value as i32;
    let matched = match raw_cmp {
        FUTEX_OP_CMP_EQ => old_value == cmparg,
        FUTEX_OP_CMP_NE => old_value != cmparg,
        FUTEX_OP_CMP_LT => old_value < cmparg,
        FUTEX_OP_CMP_LE => old_value <= cmparg,
        FUTEX_OP_CMP_GT => old_value > cmparg,
        FUTEX_OP_CMP_GE => old_value >= cmparg,
        _ => return Err(AxError::Unsupported),
    };
    Ok(matched)
}

fn futex_atomic_op_in_user(uaddr: *mut u32, encoded_op: u32) -> AxResult<bool> {
    if !uaddr.addr().is_multiple_of(align_of::<u32>()) {
        return Err(AxError::InvalidInput);
    }

    let raw_op = (encoded_op >> 28) & 0xf;
    let raw_cmp = (encoded_op >> 24) & 0xf;
    let oparg = futex_wake_op_arg(raw_op, encoded_op);
    let cmparg = sign_extend_12(encoded_op & 0xfff);

    let old_value = atomic_update_user_u32(uaddr, |old_value| {
        apply_futex_wake_op(old_value, raw_op, oparg)
    })?;
    compare_futex_wake_op(old_value, raw_cmp, cmparg)
}

fn parse_futex_op(futex_op: u32) -> AxResult<ParsedFutexOp> {
    let flags = futex_op & !FUTEX_COMMAND_MASK;
    if flags & !SUPPORTED_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let command = match futex_op & FUTEX_COMMAND_MASK {
        FUTEX_WAIT => FutexCommand::Wait,
        FUTEX_WAKE => FutexCommand::Wake,
        FUTEX_WAIT_BITSET => FutexCommand::WaitBitset,
        FUTEX_WAKE_BITSET => FutexCommand::WakeBitset,
        FUTEX_REQUEUE => FutexCommand::Requeue,
        FUTEX_CMP_REQUEUE => FutexCommand::CmpRequeue,
        FUTEX_WAKE_OP => FutexCommand::WakeOp,
        _ => return Err(AxError::Unsupported),
    };

    let clock_realtime = flags & FUTEX_CLOCK_REALTIME != 0;
    if clock_realtime && command == FutexCommand::WakeOp {
        return Err(AxError::Unsupported);
    }
    if clock_realtime && !matches!(command, FutexCommand::Wait | FutexCommand::WaitBitset) {
        return Err(AxError::InvalidInput);
    }

    let key_mode = if flags & FUTEX_PRIVATE_FLAG != 0 {
        FutexKeyMode::Private
    } else {
        FutexKeyMode::Auto
    };

    Ok(ParsedFutexOp {
        command,
        key_mode,
        clock_realtime,
    })
}

fn futex_wait_timeout(op: &ParsedFutexOp, timeout: *const timespec) -> AxResult<Option<TimeValue>> {
    let Some(ts) = timeout.nullable() else {
        return Ok(None);
    };

    let timeout = unsafe { ts.vm_read_uninit()?.assume_init() }.try_into_time_value()?;
    // FUTEX_WAIT keeps the traditional relative timeout. FUTEX_WAIT_BITSET
    // uses an absolute deadline on the selected clock.
    if op.command == FutexCommand::Wait {
        return Ok(Some(timeout));
    }

    let now = if op.clock_realtime {
        wall_time()
    } else {
        monotonic_time()
    };

    Ok(Some(timeout.saturating_sub(now)))
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

    let op = parse_futex_op(futex_op)?;
    if !uaddr.addr().is_multiple_of(align_of::<u32>()) {
        return Err(AxError::InvalidInput);
    }
    if matches!(
        op.command,
        FutexCommand::WaitBitset | FutexCommand::WakeBitset
    ) && value3 == 0
    {
        return Err(AxError::InvalidInput);
    }

    let key = FutexKey::new_current(uaddr.addr(), op.key_mode);

    let futex_table = futex_table_for(&key);

    match op.command {
        FutexCommand::Wait | FutexCommand::WaitBitset => {
            // Fast path
            if uaddr.vm_read()? != value {
                return Err(AxError::WouldBlock);
            }

            let requested_timeout = futex_wait_timeout(&op, timeout)?;

            ax_task::yield_now();
            if uaddr.vm_read()? != value {
                return Err(AxError::WouldBlock);
            }

            let futex = futex_table.get_or_insert(&key);
            let cleanup = futex_table.cleanup_for(&key);

            let bitset = if op.command == FutexCommand::WaitBitset {
                value3
            } else {
                u32::MAX
            };

            let wait_timeout = requested_timeout.or(Some(TimeValue::from_millis(1)));
            let wait_result =
                futex
                    .wq
                    .wait_if_with_cleanup(bitset, wait_timeout, Some(cleanup), || {
                        uaddr.vm_read() == Ok(value)
                    });
            let waited = match wait_result {
                Ok(waited) => waited,
                Err(AxError::TimedOut) if requested_timeout.is_none() => true,
                Err(err) => return Err(err),
            };

            if !waited {
                return Err(AxError::WouldBlock);
            }

            Ok(0)
        }
        FutexCommand::Wake | FutexCommand::WakeBitset => {
            let wake_count = assert_non_negative_i32(value)? as usize;
            validate_futex_word(uaddr)?;

            let futex = futex_table.get(&key);
            let mut count = 0;
            if let Some(futex) = futex {
                let bitset = if op.command == FutexCommand::WakeBitset {
                    value3
                } else {
                    u32::MAX
                };
                count = futex.wq.wake(wake_count, bitset);
            }
            ax_task::yield_now();
            Ok(count as _)
        }
        FutexCommand::Requeue | FutexCommand::CmpRequeue => {
            let wake_count = assert_non_negative_i32(value)? as usize;
            let requeue_count = assert_non_negative_i32(timeout.addr() as u32)? as usize;
            if op.command == FutexCommand::Requeue {
                validate_futex_word(uaddr)?;
            }
            validate_futex_word(uaddr2)?;

            let key2 = FutexKey::new_current(uaddr2.addr(), op.key_mode);
            let table2 = futex_table_for(&key2);
            let target = table2.get_or_insert(&key2);
            let target_cleanup = table2.cleanup_for(&key2);

            let Some(source) = futex_table.get(&key) else {
                if op.command == FutexCommand::CmpRequeue && uaddr.vm_read()? != value3 {
                    return Err(AxError::WouldBlock);
                }
                return Ok(0);
            };

            let count = source.wq.wake_requeue_if(
                wake_count,
                u32::MAX,
                requeue_count,
                target_cleanup,
                &target.wq,
                || {
                    if op.command == FutexCommand::CmpRequeue {
                        Ok(uaddr.vm_read()? == value3)
                    } else {
                        Ok(true)
                    }
                },
            )?;

            let Some(count) = count else {
                return Err(AxError::WouldBlock);
            };

            if count > 0 {
                ax_task::yield_now();
            }
            Ok(count as _)
        }
        FutexCommand::WakeOp => {
            let wake_count = value as usize;
            let wake2_count = timeout.addr();
            validate_futex_word(uaddr)?;

            let key2 = FutexKey::new_current(uaddr2.addr(), op.key_mode);
            let table2 = futex_table_for(&key2);

            let source = futex_table.get_or_insert(&key);
            let target = table2.get_or_insert(&key2);
            let count = source.wq.wake_op(wake_count, &target.wq, wake2_count, || {
                futex_atomic_op_in_user(uaddr2, value3)
            })?;

            if count > 0 {
                ax_task::yield_now();
            }
            Ok(count as _)
        }
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
