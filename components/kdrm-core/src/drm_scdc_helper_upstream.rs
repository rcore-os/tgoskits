// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/display/drm_scdc_helper.c`.
//!
//! Implements Status and Control Data Channel (SCDC) operations over I2C:
//! reading/writing SCDC registers, enabling/disabling scrambling, setting
//! the TMDS 1/40 vs 1/10 bit clock ratio, and reading scrambling status.

#![allow(non_snake_case)]

use core::ffi::c_void;

use drm_kapi::{
    config::{ENOMEM, EOPNOTSUPP, EPROTO, GFP_KERNEL},
    i2c::{I2C_M_RD, I2cAdapter, I2cMsg},
    memory::{kfree, kmalloc},
};

use crate::drm_connector_types::DrmConnector;

#[inline]
fn usleep_range(min_us: u64, max_us: u64) {
    #[cfg(not(test))]
    drm_kapi::iopoll::usleep_range(min_us, max_us);
    #[cfg(test)]
    let _ = (min_us, max_us);
}

// SCDC slave address and register definitions from Linux drm_scdc.h / drm_scdc_helper.h
pub const SCDC_I2C_SLAVE_ADDRESS: u16 = 0x54;

pub const SCDC_SINK_VERSION: u8 = 0x01;
pub const SCDC_SOURCE_VERSION: u8 = 0x02;

pub const SCDC_UPDATE_0: u8 = 0x10;
pub const SCDC_READ_REQUEST_TEST: u8 = 1 << 2;
pub const SCDC_CED_UPDATE: u8 = 1 << 1;
pub const SCDC_STATUS_UPDATE: u8 = 1 << 0;

pub const SCDC_UPDATE_1: u8 = 0x11;

pub const SCDC_TMDS_CONFIG: u8 = 0x20;
pub const SCDC_TMDS_BIT_CLOCK_RATIO_BY_40: u8 = 1 << 1;
pub const SCDC_TMDS_BIT_CLOCK_RATIO_BY_10: u8 = 0 << 1;
pub const SCDC_SCRAMBLING_ENABLE: u8 = 1 << 0;

pub const SCDC_SCRAMBLER_STATUS: u8 = 0x21;
pub const SCDC_SCRAMBLING_STATUS: u8 = 1 << 0;

pub const SCDC_CONFIG_0: u8 = 0x30;
pub const SCDC_READ_REQUEST_ENABLE: u8 = 1 << 0;

pub const SCDC_STATUS_FLAGS_0: u8 = 0x40;
pub const SCDC_CH2_LOCK: u8 = 1 << 3;
pub const SCDC_CH1_LOCK: u8 = 1 << 2;
pub const SCDC_CH0_LOCK: u8 = 1 << 1;
pub const SCDC_CH_LOCK_MASK: u8 = SCDC_CH2_LOCK | SCDC_CH1_LOCK | SCDC_CH0_LOCK;
pub const SCDC_CLOCK_DETECT: u8 = 1 << 0;

pub const SCDC_STATUS_FLAGS_1: u8 = 0x41;

macro_rules! drm_dbg_kms {
    ($device:expr, $format:expr $(, $argument:expr)* $(,)?) => {
        drm_kapi::drm_dbg!($device, $format $(, $argument)*)
    };
}

/// Helper calling I2cAdapter transfer.
///
/// # Safety
///
/// If non-null, `adapter` must point to an active, valid `I2cAdapter` instance.
unsafe fn i2c_transfer(adapter: *mut c_void, msgs: &mut [I2cMsg<'_>]) -> i32 {
    if adapter.is_null() {
        return -EOPNOTSUPP;
    }
    // SAFETY: `adapter` was verified non-null and caller guarantees it points to a valid `I2cAdapter`.
    let adap = unsafe { &*(adapter as *const I2cAdapter<'static>) };
    adap.transfer(msgs)
}

// upstream: drivers/gpu/drm/display/drm_scdc_helper.c drm_scdc_read()
/// # Safety
///
/// - If non-null, `adapter` must point to a valid `I2cAdapter`.
/// - If `size > 0`, `buffer` must point to a writable memory region of at least `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_scdc_read(
    adapter: *mut c_void,
    offset: u8,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if adapter.is_null() || buffer.is_null() {
        return -drm_kapi::config::EINVAL as isize;
    }

    let mut off_buf = [offset];
    // SAFETY: `buffer` is non-null and caller guarantees it points to a writable buffer of length `size`.
    let rd_buf = unsafe { core::slice::from_raw_parts_mut(buffer as *mut u8, size) };
    let mut msgs = [
        I2cMsg::new(SCDC_I2C_SLAVE_ADDRESS, 0, &mut off_buf),
        I2cMsg::new(SCDC_I2C_SLAVE_ADDRESS, I2C_M_RD, rd_buf),
    ];

    // SAFETY: `adapter` is non-null and points to a valid `I2cAdapter`.
    let ret = unsafe { i2c_transfer(adapter, &mut msgs) };
    if ret < 0 {
        return ret as isize;
    }
    if ret != 2 {
        return -EPROTO as isize;
    }

    0
}

// upstream: drivers/gpu/drm/display/drm_scdc_helper.c drm_scdc_write()
/// # Safety
///
/// - If non-null, `adapter` must point to a valid `I2cAdapter`.
/// - If `size > 0`, `buffer` must point to an accessible, readable memory region of at least `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_scdc_write(
    adapter: *mut c_void,
    offset: u8,
    buffer: *const c_void,
    size: usize,
) -> isize {
    if adapter.is_null() || (size > 0 && buffer.is_null()) {
        return -drm_kapi::config::EINVAL as isize;
    }

    let total_len = 1 + size;
    let data = kmalloc(total_len, GFP_KERNEL);
    if data.is_null() {
        return -ENOMEM as isize;
    }

    // SAFETY: `data` was allocated with size `total_len = 1 + size`.
    // Writing `offset` at index 0 and copying `size` bytes from `buffer` to index 1 is in-bounds.
    unsafe {
        *(data as *mut u8) = offset;
        if size > 0 {
            core::ptr::copy_nonoverlapping(buffer as *const u8, (data as *mut u8).add(1), size);
        }
    }

    // SAFETY: `data` points to initialized heap memory of length `total_len`.
    let data_slice = unsafe { core::slice::from_raw_parts_mut(data as *mut u8, total_len) };
    let mut msgs = [I2cMsg::new(SCDC_I2C_SLAVE_ADDRESS, 0, data_slice)];

    // SAFETY: `adapter` was checked non-null and points to a valid `I2cAdapter`.
    let err = unsafe { i2c_transfer(adapter, &mut msgs) };

    // SAFETY: `data` was returned from `kmalloc` above; deallocating it cleans up the buffer.
    unsafe { kfree(data) };

    if err < 0 {
        return err as isize;
    }
    if err != 1 {
        return -EPROTO as isize;
    }

    0
}

// upstream: include/drm/display/drm_scdc_helper.h drm_scdc_readb()
/// # Safety
///
/// - If non-null, `adapter` must point to a valid `I2cAdapter`.
/// - `value` must point to a writable `u8`.
pub unsafe extern "C" fn drm_scdc_readb(adapter: *mut c_void, offset: u8, value: *mut u8) -> i32 {
    // SAFETY: Forwarding 1-byte read request to `drm_scdc_read`; `value` is valid for 1 byte.
    unsafe { drm_scdc_read(adapter, offset, value as *mut c_void, 1) as i32 }
}

// upstream: include/drm/display/drm_scdc_helper.h drm_scdc_writeb()
/// # Safety
///
/// If non-null, `adapter` must point to a valid `I2cAdapter`.
pub unsafe extern "C" fn drm_scdc_writeb(adapter: *mut c_void, offset: u8, value: u8) -> i32 {
    let val_buf = value;
    // SAFETY: `val_buf` is a local 1-byte buffer passed with size 1 to `drm_scdc_write`.
    unsafe { drm_scdc_write(adapter, offset, &val_buf as *const u8 as *const c_void, 1) as i32 }
}

// upstream: drivers/gpu/drm/display/drm_scdc_helper.c drm_scdc_get_scrambling_status()
/// # Safety
///
/// If non-null, `connector` must point to an active, initialized `DrmConnector` with a valid `ddc` adapter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_scdc_get_scrambling_status(connector: *mut DrmConnector) -> bool {
    if connector.is_null() {
        return false;
    }
    let mut status: u8 = 0;
    // SAFETY: `connector` was checked non-null and points to a valid `DrmConnector`.
    // `ddc` is the associated I2cAdapter pointer, and `&mut status` is a valid 1-byte buffer.
    let ret = unsafe { drm_scdc_readb((*connector).ddc, SCDC_SCRAMBLER_STATUS, &mut status) };
    if ret < 0 {
        // SAFETY: `connector` is non-null and valid. Reading fields `dev`, `base.id`, and `name`
        // is valid while the connector is active. If `name` is non-null, it is a NUL-terminated C string.
        unsafe {
            let dev = (*connector).dev;
            let id = (*connector).base.id;
            let name_str = if (*connector).name.is_null() {
                "unknown"
            } else {
                core::ffi::CStr::from_ptr((*connector).name)
                    .to_str()
                    .unwrap_or("unknown")
            };
            drm_dbg_kms!(
                dev as *const c_void,
                "[CONNECTOR:{}:{}] Failed to read scrambling status: {}\n",
                id,
                name_str,
                ret
            );
        }
        return false;
    }

    (status & SCDC_SCRAMBLING_STATUS) != 0
}

// upstream: drivers/gpu/drm/display/drm_scdc_helper.c drm_scdc_set_scrambling()
/// # Safety
///
/// If non-null, `connector` must point to an active, initialized `DrmConnector` with a valid `ddc` adapter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_scdc_set_scrambling(
    connector: *mut DrmConnector,
    enable: bool,
) -> bool {
    if connector.is_null() {
        return false;
    }
    let mut config: u8 = 0;
    // SAFETY: `connector` was checked non-null and points to a valid `DrmConnector`.
    // `ddc` is the associated I2cAdapter pointer, and `&mut config` is a valid 1-byte buffer.
    let ret = unsafe { drm_scdc_readb((*connector).ddc, SCDC_TMDS_CONFIG, &mut config) };
    if ret < 0 {
        // SAFETY: `connector` is non-null and valid. Reading connector fields for debugging.
        unsafe {
            let dev = (*connector).dev;
            let id = (*connector).base.id;
            let name_str = if (*connector).name.is_null() {
                "unknown"
            } else {
                core::ffi::CStr::from_ptr((*connector).name)
                    .to_str()
                    .unwrap_or("unknown")
            };
            drm_dbg_kms!(
                dev as *const c_void,
                "[CONNECTOR:{}:{}] Failed to read TMDS config: {}\n",
                id,
                name_str,
                ret
            );
        }
        return false;
    }

    if enable {
        config |= SCDC_SCRAMBLING_ENABLE;
    } else {
        config &= !SCDC_SCRAMBLING_ENABLE;
    }

    // SAFETY: `connector` is valid; writing updated configuration byte to DDC SCDC_TMDS_CONFIG.
    let ret = unsafe { drm_scdc_writeb((*connector).ddc, SCDC_TMDS_CONFIG, config) };
    if ret < 0 {
        // SAFETY: `connector` is non-null and valid. Reading connector fields for debugging.
        unsafe {
            let dev = (*connector).dev;
            let id = (*connector).base.id;
            let name_str = if (*connector).name.is_null() {
                "unknown"
            } else {
                core::ffi::CStr::from_ptr((*connector).name)
                    .to_str()
                    .unwrap_or("unknown")
            };
            drm_dbg_kms!(
                dev as *const c_void,
                "[CONNECTOR:{}:{}] Failed to enable scrambling: {}\n",
                id,
                name_str,
                ret
            );
        }
        return false;
    }

    true
}

// upstream: drivers/gpu/drm/display/drm_scdc_helper.c drm_scdc_set_high_tmds_clock_ratio()
/// # Safety
///
/// If non-null, `connector` must point to an active, initialized `DrmConnector` with a valid `ddc` adapter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drm_scdc_set_high_tmds_clock_ratio(
    connector: *mut DrmConnector,
    set: bool,
) -> bool {
    if connector.is_null() {
        return false;
    }
    let mut config: u8 = 0;
    // SAFETY: `connector` was checked non-null and points to a valid `DrmConnector`.
    // `ddc` is the associated I2cAdapter pointer, and `&mut config` is a valid 1-byte buffer.
    let ret = unsafe { drm_scdc_readb((*connector).ddc, SCDC_TMDS_CONFIG, &mut config) };
    if ret < 0 {
        // SAFETY: `connector` is non-null and valid. Reading connector fields for debugging.
        unsafe {
            let dev = (*connector).dev;
            let id = (*connector).base.id;
            let name_str = if (*connector).name.is_null() {
                "unknown"
            } else {
                core::ffi::CStr::from_ptr((*connector).name)
                    .to_str()
                    .unwrap_or("unknown")
            };
            drm_dbg_kms!(
                dev as *const c_void,
                "[CONNECTOR:{}:{}] Failed to read TMDS config: {}\n",
                id,
                name_str,
                ret
            );
        }
        return false;
    }

    if set {
        config |= SCDC_TMDS_BIT_CLOCK_RATIO_BY_40;
    } else {
        config &= !SCDC_TMDS_BIT_CLOCK_RATIO_BY_40;
    }

    // SAFETY: `connector` is valid; writing updated configuration byte to DDC SCDC_TMDS_CONFIG.
    let ret = unsafe { drm_scdc_writeb((*connector).ddc, SCDC_TMDS_CONFIG, config) };
    if ret < 0 {
        // SAFETY: `connector` is non-null and valid. Reading connector fields for debugging.
        unsafe {
            let dev = (*connector).dev;
            let id = (*connector).base.id;
            let name_str = if (*connector).name.is_null() {
                "unknown"
            } else {
                core::ffi::CStr::from_ptr((*connector).name)
                    .to_str()
                    .unwrap_or("unknown")
            };
            drm_dbg_kms!(
                dev as *const c_void,
                "[CONNECTOR:{}:{}] Failed to set TMDS clock ratio: {}\n",
                id,
                name_str,
                ret
            );
        }
        return false;
    }

    usleep_range(1000, 2000);
    true
}
