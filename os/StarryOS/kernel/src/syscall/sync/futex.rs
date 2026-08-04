use core::mem::align_of;

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::{
    cpu::{UserAccessError, UserAtomicError, UserAtomicU32Op},
    time::{TimeValue, monotonic_time, wall_time},
};
use linux_raw_sys::general::{
    FUTEX_CLOCK_REALTIME, FUTEX_CMP_REQUEUE, FUTEX_OP_ADD, FUTEX_OP_ANDN, FUTEX_OP_CMP_EQ,
    FUTEX_OP_CMP_GE, FUTEX_OP_CMP_GT, FUTEX_OP_CMP_LE, FUTEX_OP_CMP_LT, FUTEX_OP_CMP_NE,
    FUTEX_OP_OPARG_SHIFT, FUTEX_OP_OR, FUTEX_OP_SET, FUTEX_OP_XOR, FUTEX_REQUEUE, FUTEX_WAIT,
    FUTEX_WAIT_BITSET, FUTEX_WAKE, FUTEX_WAKE_BITSET, FUTEX_WAKE_OP, robust_list_head, timespec,
};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    mm::{
        atomic_update_user_u32_nofault, fault_in_user_u32_read, fault_in_user_u32_write,
        read_user_u32_nofault,
    },
    task::{
        FutexAccessError, FutexContext, FutexKeyMode, FutexWaitError, current_user_task, get_task,
    },
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

#[derive(Clone, Copy)]
struct ParsedFutexWakeOp {
    operation: UserAtomicU32Op,
    argument: u32,
    comparison: u32,
    comparison_argument: i32,
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
    loop {
        match futex_read_user_nofault(uaddr) {
            Ok(_) => return Ok(()),
            Err(FutexAccessError::UserFault) => {
                fault_in_user_u32_read(uaddr)?;
                crate::task::yield_now();
            }
            Err(FutexAccessError::Retry) => crate::task::yield_now(),
            Err(FutexAccessError::Operation(error)) => return Err(error),
        }
    }
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

#[cfg(axtest)]
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

fn parse_futex_wake_op(encoded_op: u32) -> AxResult<ParsedFutexWakeOp> {
    let raw_op = (encoded_op >> 28) & 0xf;
    let raw_cmp = (encoded_op >> 24) & 0xf;
    let oparg = futex_wake_op_arg(raw_op, encoded_op);
    let cmparg = sign_extend_12(encoded_op & 0xfff);

    let operation = match raw_op & !FUTEX_OP_OPARG_SHIFT {
        FUTEX_OP_SET => UserAtomicU32Op::Set,
        FUTEX_OP_ADD => UserAtomicU32Op::Add,
        FUTEX_OP_OR => UserAtomicU32Op::Or,
        FUTEX_OP_ANDN => UserAtomicU32Op::AndNot,
        FUTEX_OP_XOR => UserAtomicU32Op::Xor,
        _ => return Err(AxError::Unsupported),
    };
    match raw_cmp {
        FUTEX_OP_CMP_EQ | FUTEX_OP_CMP_NE | FUTEX_OP_CMP_LT | FUTEX_OP_CMP_LE | FUTEX_OP_CMP_GT
        | FUTEX_OP_CMP_GE => {}
        _ => return Err(AxError::Unsupported),
    }
    Ok(ParsedFutexWakeOp {
        operation,
        argument: oparg as u32,
        comparison: raw_cmp,
        comparison_argument: cmparg,
    })
}

fn futex_atomic_op_in_user_nofault(
    uaddr: *mut u32,
    operation: ParsedFutexWakeOp,
) -> Result<bool, FutexAccessError> {
    let old_value = atomic_update_user_u32_nofault(uaddr, operation.operation, operation.argument)
        .map_err(|error| match error {
            UserAtomicError::Fault => FutexAccessError::UserFault,
            UserAtomicError::Retry => FutexAccessError::Retry,
        })?;
    compare_futex_wake_op(
        old_value,
        operation.comparison,
        operation.comparison_argument,
    )
    .map_err(Into::into)
}

fn futex_read_user_nofault(uaddr: *const u32) -> Result<u32, FutexAccessError> {
    read_user_u32_nofault(uaddr).map_err(|error| match error {
        UserAccessError::Fault => FutexAccessError::UserFault,
    })
}

fn apply_wake_op_without_waiters(uaddr: *mut u32, operation: ParsedFutexWakeOp) -> AxResult<()> {
    loop {
        match futex_atomic_op_in_user_nofault(uaddr, operation) {
            Ok(_) => return Ok(()),
            Err(FutexAccessError::UserFault) => {
                fault_in_user_u32_write(uaddr)?;
                crate::task::yield_now();
            }
            Err(FutexAccessError::Retry) => crate::task::yield_now(),
            Err(FutexAccessError::Operation(error)) => return Err(error),
        }
    }
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

fn complete_futex_wake(count: usize) -> AxResult<isize> {
    // Waker publication makes the target runnable and lets ax-task set the
    // owner CPU's sticky preemption state. Mirroring Linux wake_q completion,
    // the futex syscall must not add a second, unconditional scheduling point.
    Ok(count as isize)
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

    match op.command {
        FutexCommand::Wait | FutexCommand::WaitBitset => {
            // Fast path
            if uaddr.vm_read()? != value {
                return Err(AxError::WouldBlock);
            }

            let timeout = futex_wait_timeout(&op, timeout)?;

            let bitset = if op.command == FutexCommand::WaitBitset {
                value3
            } else {
                u32::MAX
            };
            let context = FutexContext::current();

            loop {
                let futex = context.resolve(uaddr.addr(), op.key_mode);
                let entry = futex.table().get_or_insert(futex.key());
                let cleanup = futex.table().cleanup_for(futex.key());
                match entry.wq.wait_if_with_cleanup_nofault_for(
                    context.task(),
                    bitset,
                    timeout,
                    Some(cleanup),
                    || futex_read_user_nofault(uaddr).map(|observed| observed == value),
                ) {
                    Ok(true) => break,
                    Ok(false) => return Err(AxError::WouldBlock),
                    Err(FutexWaitError::SchedulerNotification) => continue,
                    Err(FutexWaitError::Access(FutexAccessError::UserFault)) => {
                        fault_in_user_u32_read(uaddr)?;
                        crate::task::yield_now();
                    }
                    Err(FutexWaitError::Access(FutexAccessError::Retry)) => {
                        crate::task::yield_now()
                    }
                    Err(FutexWaitError::Access(FutexAccessError::Operation(error))) => {
                        return Err(error);
                    }
                }
            }

            Ok(0)
        }
        FutexCommand::Wake | FutexCommand::WakeBitset => {
            let wake_count = assert_non_negative_i32(value)? as usize;
            validate_futex_word(uaddr)?;

            let futex = FutexContext::current().resolve(uaddr.addr(), op.key_mode);
            let entry = futex.table().get(futex.key());
            let mut count = 0;
            if let Some(entry) = entry {
                let bitset = if op.command == FutexCommand::WakeBitset {
                    value3
                } else {
                    u32::MAX
                };
                count = entry.wq.wake(wake_count, bitset);
            }
            complete_futex_wake(count)
        }
        FutexCommand::Requeue | FutexCommand::CmpRequeue => {
            let wake_count = assert_non_negative_i32(value)? as usize;
            let requeue_count = assert_non_negative_i32(timeout.addr() as u32)? as usize;
            if op.command == FutexCommand::Requeue {
                validate_futex_word(uaddr)?;
            }
            validate_futex_word(uaddr2)?;
            let context = FutexContext::current();

            let count = loop {
                let (source_futex, target_futex) =
                    context.resolve_pair(uaddr.addr(), uaddr2.addr(), op.key_mode);
                let target = target_futex.table().get_or_insert(target_futex.key());
                let target_cleanup = target_futex.table().cleanup_for(target_futex.key());

                let Some(source) = source_futex.table().get(source_futex.key()) else {
                    if op.command == FutexCommand::CmpRequeue && uaddr.vm_read()? != value3 {
                        return Err(AxError::WouldBlock);
                    }
                    return Ok(0);
                };

                match source.wq.wake_requeue_if(
                    wake_count,
                    u32::MAX,
                    requeue_count,
                    target_cleanup,
                    &target.wq,
                    || {
                        if op.command == FutexCommand::CmpRequeue {
                            futex_read_user_nofault(uaddr).map(|observed| observed == value3)
                        } else {
                            Ok(true)
                        }
                    },
                ) {
                    Ok(Some(count)) => break count,
                    Ok(None) => return Err(AxError::WouldBlock),
                    Err(FutexAccessError::UserFault) => {
                        fault_in_user_u32_read(uaddr)?;
                        crate::task::yield_now();
                    }
                    Err(FutexAccessError::Retry) => crate::task::yield_now(),
                    Err(FutexAccessError::Operation(error)) => return Err(error),
                }
            };

            complete_futex_wake(count)
        }
        FutexCommand::WakeOp => {
            let wake_count = value as usize;
            let wake2_count = timeout.addr();
            validate_futex_word(uaddr)?;
            if !uaddr2.addr().is_multiple_of(align_of::<u32>()) {
                return Err(AxError::InvalidInput);
            }
            let wake_operation = parse_futex_wake_op(value3)?;

            let count = if wake_count == 0 && wake2_count == 0 {
                // No waiter state can change when both wake limits are zero.
                // The user RMW is already atomic, so taking futex table locks
                // would add PI contention without protecting any kernel data.
                apply_wake_op_without_waiters(uaddr2, wake_operation)?;
                0
            } else {
                let context = FutexContext::current();
                loop {
                    // Shared keys depend on the current VMA backing and must be
                    // recomputed after fault-in, matching Linux futex retry.
                    let (source, target) =
                        context.resolve_pair(uaddr.addr(), uaddr2.addr(), op.key_mode);
                    match source.table().wake_op(
                        source.key(),
                        wake_count,
                        target.table(),
                        target.key(),
                        wake2_count,
                        || futex_atomic_op_in_user_nofault(uaddr2, wake_operation),
                    ) {
                        Ok(count) => break count,
                        Err(FutexAccessError::UserFault) => {
                            fault_in_user_u32_write(uaddr2)?;
                            crate::task::yield_now();
                        }
                        Err(FutexAccessError::Retry) => crate::task::yield_now(),
                        Err(FutexAccessError::Operation(error)) => return Err(error),
                    }
                }
            };

            complete_futex_wake(count)
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
    current_user_task()
        .as_thread()
        .set_robust_list_head(head.addr());

    Ok(0)
}

#[cfg(axtest)]
pub(crate) fn futex_op_and_compare_rules_hold_for_test() -> bool {
    // sign_extend_12: sign-extends a 12-bit value.
    assert!(sign_extend_12(0x000) == 0);
    assert!(sign_extend_12(0x7FF) == 2047); // max positive
    assert!(sign_extend_12(0x800) == -2048); // min negative
    assert!(sign_extend_12(0xFFF) == -1);

    // futex_wake_op_arg: extracts oparg from encoded_op, optionally shifts.
    let raw_op_set = FUTEX_OP_SET;
    let encoded_no_shift = (5u32) << 12; // oparg=5, no shift
    assert!(futex_wake_op_arg(raw_op_set, encoded_no_shift) == 5);

    let raw_op_shift = FUTEX_OP_SET | FUTEX_OP_OPARG_SHIFT;
    let encoded_shift = (3u32) << 12; // oparg=3, shift by 3
    assert!(futex_wake_op_arg(raw_op_shift, encoded_shift) == 8); // 1 << 3 = 8

    // apply_futex_wake_op: applies the operation to old_value.
    assert!(apply_futex_wake_op(10, FUTEX_OP_SET, 42).unwrap() == 42);
    assert!(apply_futex_wake_op(10, FUTEX_OP_ADD, 5).unwrap() == 15);
    assert!(apply_futex_wake_op(0b1100, FUTEX_OP_OR, 0b1010).unwrap() == 0b1110);
    assert!(apply_futex_wake_op(0xFF, FUTEX_OP_ANDN, 0x0F).unwrap() == 0xF0);
    assert!(apply_futex_wake_op(0xAA, FUTEX_OP_XOR, 0xFF).unwrap() == 0x55);
    assert!(apply_futex_wake_op(0, 0xFFFF, 0).is_err()); // unsupported op

    // compare_futex_wake_op: compares old_value with cmparg.
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_EQ, 5).unwrap() == true);
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_EQ, 6).unwrap() == false);
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_NE, 6).unwrap() == true);
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_LT, 10).unwrap() == true);
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_LE, 5).unwrap() == true);
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_GT, 3).unwrap() == true);
    assert!(compare_futex_wake_op(5, FUTEX_OP_CMP_GE, 5).unwrap() == true);
    assert!(compare_futex_wake_op(0, 0xFFFF, 0).is_err()); // unsupported cmp

    true
}

#[cfg(axtest)]
pub(crate) fn futex_wake_completion_is_scheduler_driven_for_test() -> bool {
    crate::task::reset_yield_now_calls_for_test();
    let result = complete_futex_wake(1);
    result == Ok(1) && crate::task::yield_now_calls_for_test() == 0
}
