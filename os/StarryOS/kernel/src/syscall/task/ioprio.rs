//! ioprio_get(2) / ioprio_set(2) — I/O scheduling class and priority.
//!
//! StarryOS block I/O is served through the VFS page cache and the block
//! driver does not yet implement per-request priority queues. The syscalls
//! therefore validate the Linux ABI and accept the request as a no-op,
//! reporting the default `IOPRIO_CLASS_NONE` priority, which is what Linux
//! returns for processes that never called ioprio_set(2).

use crate::{StarryError, StarryResult};

/// `IOPRIO_WHO_*` selector values from `<linux/ioprio.h>`.
const IOPRIO_WHO_PROCESS: u32 = 1;
const IOPRIO_WHO_PGRP: u32 = 2;
const IOPRIO_WHO_USER: u32 = 3;

/// `IOPRIO_CLASS_*` values from `<linux/ioprio.h>`.
const IOPRIO_CLASS_NONE: u32 = 0;
const IOPRIO_CLASS_RT: u32 = 1;
const IOPRIO_CLASS_BE: u32 = 2;
const IOPRIO_CLASS_IDLE: u32 = 3;

/// Priority data field width (`IOPRIO_BITS` = 13).
const IOPRIO_DATA_MASK: u32 = (1 << 13) - 1;
/// Class field shift (`IOPRIO_CLASS_SHIFT` = 13).
const IOPRIO_CLASS_SHIFT: u32 = 13;

fn validate_which(which: u32) -> StarryResult<()> {
    if matches!(
        which,
        IOPRIO_WHO_PROCESS | IOPRIO_WHO_PGRP | IOPRIO_WHO_USER
    ) {
        Ok(())
    } else {
        Err(StarryError::InvalidInput)
    }
}

/// ioprio_get(2) — return the I/O priority of a process, process group, or
/// user. All StarryOS I/O currently uses the default best-effort class, so
/// the returned value is `IOPRIO_PRIO_VALUE(IOPRIO_CLASS_NONE, 0)`.
pub fn sys_ioprio_get(which: u32, _who: u32) -> StarryResult<isize> {
    validate_which(which)?;
    Ok(0)
}

/// ioprio_set(2) — set the I/O priority of a process, process group, or user.
///
/// The request is validated for ABI conformance (class and data field fit)
/// and accepted as a no-op until the block layer gains priority support.
pub fn sys_ioprio_set(which: u32, _who: u32, ioprio: u32) -> StarryResult<isize> {
    validate_which(which)?;
    let class = ioprio >> IOPRIO_CLASS_SHIFT;
    let data = ioprio & IOPRIO_DATA_MASK;
    if !matches!(
        class,
        IOPRIO_CLASS_NONE | IOPRIO_CLASS_RT | IOPRIO_CLASS_BE | IOPRIO_CLASS_IDLE
    ) {
        return Err(StarryError::InvalidInput);
    }
    // `IOPRIO_CLASS_NONE` carries no data in Linux.
    if class == IOPRIO_CLASS_NONE && data != 0 {
        return Err(StarryError::InvalidInput);
    }
    // The idle class carries no data either.
    if class == IOPRIO_CLASS_IDLE && data != 0 {
        return Err(StarryError::InvalidInput);
    }
    Ok(0)
}

#[cfg(test)]
pub(crate) fn ioprio_validation_rules_hold_for_test() -> bool {
    assert!(validate_which(IOPRIO_WHO_PROCESS).is_ok());
    assert!(validate_which(IOPRIO_WHO_PGRP).is_ok());
    assert!(validate_which(IOPRIO_WHO_USER).is_ok());
    assert!(validate_which(0).is_err());
    assert!(validate_which(4).is_err());
    assert!(sys_ioprio_get(IOPRIO_WHO_PROCESS, 0).unwrap() == 0);
    assert!(sys_ioprio_set(IOPRIO_WHO_PROCESS, 0, 0).is_ok());
    // BE class priority 7 is valid.
    assert!(
        sys_ioprio_set(
            IOPRIO_WHO_PROCESS,
            0,
            (IOPRIO_CLASS_BE << IOPRIO_CLASS_SHIFT) | 7
        )
        .is_ok()
    );
    // Unknown class is invalid.
    assert!(sys_ioprio_set(IOPRIO_WHO_PROCESS, 0, 4 << IOPRIO_CLASS_SHIFT).is_err());
    true
}
