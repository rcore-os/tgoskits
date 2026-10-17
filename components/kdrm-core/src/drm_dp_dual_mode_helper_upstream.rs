// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/display/drm_dp_dual_mode_helper.c`.

#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

extern crate alloc;

use alloc::vec::Vec;
use core::{
    ffi::{c_char, c_void},
    ptr,
};

use drm_kapi::{
    config::{EFAULT, EINVAL, EIO, ENOMEM, EPROTO, ETIMEDOUT},
    i2c::{I2C_M_RD, I2cAdapter, I2cMsg},
};

use crate::drm_device_types::DrmDevice;

pub const DP_DUAL_MODE_SLAVE_ADDRESS: u16 = 0x40;

pub const DP_DUAL_MODE_HDMI_ID: u8 = 0x00;
pub const DP_DUAL_MODE_HDMI_ID_LEN: usize = 16;
pub const DP_DUAL_MODE_ADAPTOR_ID: u8 = 0x10;
pub const DP_DUAL_MODE_REV_MASK: u8 = 0x07;
pub const DP_DUAL_MODE_REV_TYPE2: u8 = 0x00;
pub const DP_DUAL_MODE_TYPE_MASK: u8 = 0xf0;
pub const DP_DUAL_MODE_TYPE_TYPE2: u8 = 0xa0;
pub const DP_DUAL_MODE_TYPE_HAS_DPCD: u8 = 0x08;
pub const DP_DUAL_MODE_IEEE_OUI: u8 = 0x11;
pub const DP_DUAL_IEEE_OUI_LEN: usize = 3;
pub const DP_DUAL_DEVICE_ID: u8 = 0x14;
pub const DP_DUAL_DEVICE_ID_LEN: usize = 6;
pub const DP_DUAL_MODE_HARDWARE_REV: u8 = 0x1a;
pub const DP_DUAL_MODE_FIRMWARE_MAJOR_REV: u8 = 0x1b;
pub const DP_DUAL_MODE_FIRMWARE_MINOR_REV: u8 = 0x1c;
pub const DP_DUAL_MODE_MAX_TMDS_CLOCK: u8 = 0x1d;
pub const DP_DUAL_MODE_I2C_SPEED_CAP: u8 = 0x1e;
pub const DP_DUAL_MODE_TMDS_OEN: u8 = 0x20;
pub const DP_DUAL_MODE_TMDS_DISABLE: u8 = 0x01;
pub const DP_DUAL_MODE_HDMI_PIN_CTRL: u8 = 0x21;
pub const DP_DUAL_MODE_CEC_ENABLE: u8 = 0x01;
pub const DP_DUAL_MODE_I2C_SPEED_CTRL: u8 = 0x22;

pub const DP_DUAL_MODE_LSPCON_MODE_CHANGE: u8 = 0x40;
pub const DP_DUAL_MODE_LSPCON_CURRENT_MODE: u8 = 0x41;
pub const DP_DUAL_MODE_LSPCON_MODE_PCON: u8 = 0x1;

pub type DrmDpDualModeType = u32;
pub const DRM_DP_DUAL_MODE_NONE: DrmDpDualModeType = 0;
pub const DRM_DP_DUAL_MODE_UNKNOWN: DrmDpDualModeType = 1;
pub const DRM_DP_DUAL_MODE_TYPE1_DVI: DrmDpDualModeType = 2;
pub const DRM_DP_DUAL_MODE_TYPE1_HDMI: DrmDpDualModeType = 3;
pub const DRM_DP_DUAL_MODE_TYPE2_DVI: DrmDpDualModeType = 4;
pub const DRM_DP_DUAL_MODE_TYPE2_HDMI: DrmDpDualModeType = 5;
pub const DRM_DP_DUAL_MODE_LSPCON: DrmDpDualModeType = 6;

pub type DrmLspconMode = u32;
pub const DRM_LSPCON_MODE_INVALID: DrmLspconMode = 0;
pub const DRM_LSPCON_MODE_LS: DrmLspconMode = 1;
pub const DRM_LSPCON_MODE_PCON: DrmLspconMode = 2;

/// # Safety
///
/// If `adapter` is non-null, it must point to a live, properly aligned `I2cAdapter`
/// instance valid for the duration of this call whose underlying bus transfer method
/// is safe to invoke without data races.
unsafe fn i2c_transfer(adapter: *mut c_void, msgs: &mut [I2cMsg<'_>]) -> i32 {
    if adapter.is_null() {
        return -EINVAL;
    }
    // SAFETY: `adapter` is verified non-null and guaranteed by caller to point to a valid, live, properly aligned `I2cAdapter` whose lifetime encompasses this call.
    let adap = unsafe { &*(adapter as *const I2cAdapter<'static>) };
    adap.transfer(msgs)
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_dual_mode_read()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `adapter` may be null (returns `-EINVAL`). If non-null, it must point to a live, properly aligned `I2cAdapter` instance valid for I2C transfers for the duration of the call.
/// - If `size > 0`, `buffer` must be non-null and point to a writable byte buffer of at least `size` bytes, not aliased by any concurrent reader or writer.
/// - If `size == 0`, `buffer` is ignored and may be null or dangling.
pub unsafe extern "C" fn drm_dp_dual_mode_read(
    adapter: *mut c_void,
    offset: u8,
    buffer: *mut c_void,
    size: usize,
) -> isize {
    if adapter.is_null() || (size > 0 && buffer.is_null()) {
        return -EINVAL as isize;
    }
    let mut zero: [u8; 1] = [0];
    let total_read_len = size + offset as usize;
    let mut tmpbuf: Vec<u8> = Vec::new();
    if tmpbuf.try_reserve_exact(total_read_len).is_err() {
        return -ENOMEM as isize;
    }
    tmpbuf.resize(total_read_len, 0);

    let msg0 = I2cMsg::new(DP_DUAL_MODE_SLAVE_ADDRESS, 0, &mut zero);
    let msg1 = I2cMsg::new(DP_DUAL_MODE_SLAVE_ADDRESS, I2C_M_RD, &mut tmpbuf[..]);
    let mut msgs = [msg0, msg1];

    // SAFETY: `adapter` is live or null per the function's safety contract (`i2c_transfer` rejects null); `msgs` is a local array of two valid messages.
    let ret = unsafe { i2c_transfer(adapter, &mut msgs) };
    if ret < 0 {
        return ret as isize;
    }
    if ret != 2 {
        return -EPROTO as isize;
    }

    if size > 0 {
        // SAFETY: `tmpbuf` has length `size + offset`, so offset..offset + size is within bounds; `buffer` is verified non-null and valid for writing `size` bytes per function contract without aliasing `tmpbuf`.
        unsafe {
            ptr::copy_nonoverlapping(
                tmpbuf.as_ptr().add(offset as usize),
                buffer as *mut u8,
                size,
            );
        }
    }
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_dual_mode_write()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `adapter` may be null (returns `-EINVAL`). If non-null, it must point to a live, properly aligned `I2cAdapter` instance valid for I2C transfers for the duration of the call.
/// - If `size > 0`, `buffer` must be non-null and point to a readable byte buffer of at least `size` bytes that remains unmodified for the duration of the call.
/// - If `size == 0`, `buffer` is ignored and may be null or dangling.
pub unsafe extern "C" fn drm_dp_dual_mode_write(
    adapter: *mut c_void,
    offset: u8,
    buffer: *const c_void,
    size: usize,
) -> isize {
    if adapter.is_null() || (size > 0 && buffer.is_null()) {
        return -EINVAL as isize;
    }
    let mut data: Vec<u8> = Vec::new();
    if data.try_reserve_exact(1 + size).is_err() {
        return -ENOMEM as isize;
    }
    data.push(offset);
    if size > 0 {
        // SAFETY: `buffer` is verified non-null and points to at least `size` readable bytes with no concurrent writes per function contract.
        let src = unsafe { core::slice::from_raw_parts(buffer as *const u8, size) };
        data.extend_from_slice(src);
    }

    let mut msg = I2cMsg::new(DP_DUAL_MODE_SLAVE_ADDRESS, 0, &mut data[..]);
    // SAFETY: `adapter` is live or null per the function's safety contract (`i2c_transfer` rejects null); `msg` is a valid local message.
    let ret = unsafe { i2c_transfer(adapter, core::slice::from_mut(&mut msg)) };
    if ret < 0 {
        return ret as isize;
    }
    if ret != 1 {
        return -EPROTO as isize;
    }
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c is_hdmi_adaptor()
fn is_hdmi_adaptor(hdmi_id: &[u8; DP_DUAL_MODE_HDMI_ID_LEN]) -> bool {
    hdmi_id == b"DP-HDMI ADAPTOR\x04"
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c is_type1_adaptor()
fn is_type1_adaptor(adaptor_id: u8) -> bool {
    adaptor_id == 0 || adaptor_id == 0xff
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c is_type2_adaptor()
fn is_type2_adaptor(adaptor_id: u8) -> bool {
    adaptor_id == (DP_DUAL_MODE_TYPE_TYPE2 | DP_DUAL_MODE_REV_TYPE2)
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c is_lspcon_adaptor()
fn is_lspcon_adaptor(hdmi_id: &[u8; DP_DUAL_MODE_HDMI_ID_LEN], adaptor_id: u8) -> bool {
    is_hdmi_adaptor(hdmi_id)
        && (adaptor_id == (DP_DUAL_MODE_TYPE_TYPE2 | DP_DUAL_MODE_TYPE_HAS_DPCD))
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_dual_mode_detect()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `adapter` must be a valid pointer to a live, properly aligned `I2cAdapter` capable of executing dual-mode I2C transfers, or null (in which case detection fails and returns `DRM_DP_DUAL_MODE_UNKNOWN`).
/// - `dev` may be null or point to a live `DrmDevice` used for diagnostic error logging (`drm_err!`).
pub unsafe extern "C" fn drm_dp_dual_mode_detect(
    dev: *const DrmDevice,
    adapter: *mut c_void,
) -> DrmDpDualModeType {
    let mut hdmi_id = [0u8; DP_DUAL_MODE_HDMI_ID_LEN];
    let mut adaptor_id = 0u8;

    // SAFETY: `adapter` is live or null per the function's safety contract (the callee rejects null); `hdmi_id` is a local 16-byte buffer and its length is passed as `size`.
    let ret = unsafe {
        drm_dp_dual_mode_read(
            adapter,
            DP_DUAL_MODE_HDMI_ID,
            hdmi_id.as_mut_ptr() as *mut c_void,
            hdmi_id.len(),
        )
    };
    if ret != 0 {
        return DRM_DP_DUAL_MODE_UNKNOWN;
    }

    // SAFETY: `adapter` is live per the function's safety contract (the callee rejects null); `adaptor_id` is a valid local byte and the length is 1.
    let ret = unsafe {
        drm_dp_dual_mode_read(
            adapter,
            DP_DUAL_MODE_ADAPTOR_ID,
            &mut adaptor_id as *mut u8 as *mut c_void,
            1,
        )
    };
    if ret == 0 {
        if is_lspcon_adaptor(&hdmi_id, adaptor_id) {
            return DRM_DP_DUAL_MODE_LSPCON;
        }
        if is_type2_adaptor(adaptor_id) {
            if is_hdmi_adaptor(&hdmi_id) {
                return DRM_DP_DUAL_MODE_TYPE2_HDMI;
            } else {
                return DRM_DP_DUAL_MODE_TYPE2_DVI;
            }
        }
        if !is_type1_adaptor(adaptor_id) {
            drm_kapi::drm_err!(dev, "Unexpected DP dual mode adaptor ID %02x\n", adaptor_id);
        }
    }

    if is_hdmi_adaptor(&hdmi_id) {
        DRM_DP_DUAL_MODE_TYPE1_HDMI
    } else {
        DRM_DP_DUAL_MODE_TYPE1_DVI
    }
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_dual_mode_max_tmds_clock()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `type_ >= DRM_DP_DUAL_MODE_TYPE2_DVI`, `adapter` must point to a live, properly aligned `I2cAdapter` to read adaptor registers; if `type_ < DRM_DP_DUAL_MODE_TYPE2_DVI`, `adapter` is unused and may be null or dangling.
/// - `dev` may be null or point to a live `DrmDevice` used for debug logging (`drm_dbg!`).
pub unsafe extern "C" fn drm_dp_dual_mode_max_tmds_clock(
    dev: *const DrmDevice,
    type_: DrmDpDualModeType,
    adapter: *mut c_void,
) -> i32 {
    let mut max_tmds_clock = 0u8;

    if type_ == DRM_DP_DUAL_MODE_NONE {
        return 0;
    }

    if type_ < DRM_DP_DUAL_MODE_TYPE2_DVI {
        return 165000;
    }

    // SAFETY: `type_ >= TYPE2_DVI` here, so `adapter` is live per the function's safety contract; `max_tmds_clock` is a valid local byte and the length is 1.
    let ret = unsafe {
        drm_dp_dual_mode_read(
            adapter,
            DP_DUAL_MODE_MAX_TMDS_CLOCK,
            &mut max_tmds_clock as *mut u8 as *mut c_void,
            1,
        )
    };
    if ret != 0 || max_tmds_clock == 0x00 || max_tmds_clock == 0xff {
        drm_kapi::drm_dbg!(dev, "Failed to query max TMDS clock\n");
        return 165000;
    }

    (max_tmds_clock as i32) * 5000 / 2
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_dual_mode_get_tmds_output()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `enabled` may be null (returns `-EINVAL`). If non-null, it must be properly aligned and point to a writable `bool` with exclusive access (no concurrent reads or writes).
/// - If `type_ >= DRM_DP_DUAL_MODE_TYPE2_DVI`, `adapter` must point to a live, properly aligned `I2cAdapter` to query register state; if `type_ < DRM_DP_DUAL_MODE_TYPE2_DVI`, `adapter` is unused and may be null or dangling.
/// - `dev` may be null or point to a live `DrmDevice` used for debug logging (`drm_dbg!`).
pub unsafe extern "C" fn drm_dp_dual_mode_get_tmds_output(
    dev: *const DrmDevice,
    type_: DrmDpDualModeType,
    adapter: *mut c_void,
    enabled: *mut bool,
) -> i32 {
    if enabled.is_null() {
        return -EINVAL;
    }
    if type_ < DRM_DP_DUAL_MODE_TYPE2_DVI {
        // SAFETY: `enabled` was checked non-null above and per the function's safety contract is a writable, exclusively held `bool`.
        unsafe { *enabled = true };
        return 0;
    }

    let mut tmds_oen = 0u8;
    // SAFETY: `type_ >= TYPE2_DVI` here, so `adapter` is live per the function's safety contract; `tmds_oen` is a valid local byte and the length is 1.
    let ret = unsafe {
        drm_dp_dual_mode_read(
            adapter,
            DP_DUAL_MODE_TMDS_OEN,
            &mut tmds_oen as *mut u8 as *mut c_void,
            1,
        )
    };
    if ret != 0 {
        drm_kapi::drm_dbg!(dev, "Failed to query state of TMDS output buffers\n");
        return ret as i32;
    }

    // SAFETY: `enabled` was checked non-null above and per the function's safety contract is a writable, exclusively held `bool`.
    unsafe { *enabled = (tmds_oen & DP_DUAL_MODE_TMDS_DISABLE) == 0 };
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_dual_mode_set_tmds_output()
#[unsafe(no_mangle)]
/// # Safety
///
/// - If `type_ >= DRM_DP_DUAL_MODE_TYPE2_DVI`, `adapter` must point to a live, properly aligned `I2cAdapter` valid for I2C register read and write operations; if `type_ < DRM_DP_DUAL_MODE_TYPE2_DVI`, `adapter` is unused and may be null or dangling.
/// - `dev` may be null or point to a live `DrmDevice` used for debug logging (`drm_dbg!`).
pub unsafe extern "C" fn drm_dp_dual_mode_set_tmds_output(
    dev: *const DrmDevice,
    type_: DrmDpDualModeType,
    adapter: *mut c_void,
    enable: bool,
) -> i32 {
    let tmds_oen: u8 = if enable { 0 } else { DP_DUAL_MODE_TMDS_DISABLE };

    if type_ < DRM_DP_DUAL_MODE_TYPE2_DVI {
        return 0;
    }

    for retry in 0..3 {
        let mut tmp = 0u8;

        // SAFETY: `adapter` is live per the function's safety contract (`type_ >= TYPE2_DVI` here); `tmds_oen` is a valid local byte and the length is 1.
        let ret = unsafe {
            drm_dp_dual_mode_write(
                adapter,
                DP_DUAL_MODE_TMDS_OEN,
                &tmds_oen as *const u8 as *const c_void,
                1,
            )
        };
        if ret != 0 {
            drm_kapi::drm_dbg!(
                dev,
                "Failed to {} TMDS output buffers ({} attempts)\n",
                if enable { "enable" } else { "disable" },
                retry + 1
            );
            return ret as i32;
        }

        // SAFETY: `adapter` is live per the function's safety contract; `tmp` is a valid local byte and the length is 1.
        let ret = unsafe {
            drm_dp_dual_mode_read(
                adapter,
                DP_DUAL_MODE_TMDS_OEN,
                &mut tmp as *mut u8 as *mut c_void,
                1,
            )
        };
        if ret != 0 {
            drm_kapi::drm_dbg!(
                dev,
                "I2C read failed during TMDS output buffer {} ({} attempts)\n",
                if enable { "enabling" } else { "disabling" },
                retry + 1
            );
            return ret as i32;
        }

        if tmp == tmds_oen {
            return 0;
        }
    }

    drm_kapi::drm_dbg!(
        dev,
        "I2C write value mismatch during TMDS output buffer {}\n",
        if enable { "enabling" } else { "disabling" }
    );
    -EIO
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_dp_get_dual_mode_type_name()
#[unsafe(no_mangle)]
/// # Safety
///
/// This function takes `type_` by value and performs no dereferencing of raw pointers.
/// Calling this function is always safe; it is marked `unsafe extern "C"` to conform to the C ABI entry point.
/// The returned pointer references a static NUL-terminated C string with `'static` lifetime that must not be modified or freed.
pub unsafe extern "C" fn drm_dp_get_dual_mode_type_name(type_: DrmDpDualModeType) -> *const c_char {
    match type_ {
        DRM_DP_DUAL_MODE_NONE => c"none".as_ptr(),
        DRM_DP_DUAL_MODE_TYPE1_DVI => c"type 1 DVI".as_ptr(),
        DRM_DP_DUAL_MODE_TYPE1_HDMI => c"type 1 HDMI".as_ptr(),
        DRM_DP_DUAL_MODE_TYPE2_DVI => c"type 2 DVI".as_ptr(),
        DRM_DP_DUAL_MODE_TYPE2_HDMI => c"type 2 HDMI".as_ptr(),
        DRM_DP_DUAL_MODE_LSPCON => c"lspcon".as_ptr(),
        _ => c"unknown".as_ptr(),
    }
}

#[inline]
fn usleep_range(min: u64, max: u64) {
    #[cfg(not(test))]
    drm_kapi::iopoll::usleep_range(min, max);
    #[cfg(test)]
    let _ = (min, max);
}

#[inline]
fn msleep(msecs: u32) {
    #[cfg(not(test))]
    drm_kapi::wait::msleep(msecs);
    #[cfg(test)]
    let _ = msecs;
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_lspcon_get_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `mode` may be null (returns `-EINVAL`). If non-null, it must be properly aligned and point to a writable `DrmLspconMode` (`u32`) location held exclusively with no concurrent access.
/// - `adapter` must point to a live, properly aligned `I2cAdapter` capable of executing I2C transfers.
/// - `dev` may be null or point to a live `DrmDevice` used for logging (`drm_err!` / `drm_dbg!`).
pub unsafe extern "C" fn drm_lspcon_get_mode(
    dev: *const DrmDevice,
    adapter: *mut c_void,
    mode: *mut DrmLspconMode,
) -> i32 {
    if mode.is_null() {
        drm_kapi::drm_err!(dev, "NULL input\n");
        return -EINVAL;
    }

    let mut data = 0u8;
    let mut ret = 0isize;

    for retry in 0..6 {
        if retry > 0 {
            usleep_range(500, 1000);
        }
        // SAFETY: `adapter` is live per the function's safety contract; `data` is a valid local byte and the length is 1.
        ret = unsafe {
            drm_dp_dual_mode_read(
                adapter,
                DP_DUAL_MODE_LSPCON_CURRENT_MODE,
                &mut data as *mut u8 as *mut c_void,
                1,
            )
        };
        if ret == 0 {
            break;
        }
    }

    if ret < 0 {
        drm_kapi::drm_dbg!(dev, "LSPCON read(0x80, 0x41) failed\n");
        return -EFAULT;
    }

    if (data & DP_DUAL_MODE_LSPCON_MODE_PCON) != 0 {
        // SAFETY: `mode` was checked non-null above and per the function's safety contract is a writable, exclusively held `u32`.
        unsafe { *mode = DRM_LSPCON_MODE_PCON };
    } else {
        // SAFETY: `mode` was checked non-null above and per the function's safety contract is a writable, exclusively held `u32`.
        unsafe { *mode = DRM_LSPCON_MODE_LS };
    }
    0
}

// upstream: drivers/gpu/drm/display/drm_dp_dual_mode_helper.c drm_lspcon_set_mode()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `adapter` must point to a live, properly aligned `I2cAdapter` capable of executing I2C transfers for the duration of this call and its polling loops.
/// - `dev` may be null or point to a live `DrmDevice` used for logging (`drm_err!` / `drm_dbg!`).
pub unsafe extern "C" fn drm_lspcon_set_mode(
    dev: *const DrmDevice,
    adapter: *mut c_void,
    mode: DrmLspconMode,
    mut time_out: i32,
) -> i32 {
    let mut data = 0u8;
    if mode == DRM_LSPCON_MODE_PCON {
        data = DP_DUAL_MODE_LSPCON_MODE_PCON;
    }

    // SAFETY: `adapter` is live for this call per the function's safety contract; `data` is a valid local byte and the length passed is 1.
    let ret = unsafe {
        drm_dp_dual_mode_write(
            adapter,
            DP_DUAL_MODE_LSPCON_MODE_CHANGE,
            &data as *const u8 as *const c_void,
            1,
        )
    };
    if ret < 0 {
        drm_kapi::drm_err!(dev, "LSPCON mode change failed\n");
        return ret as i32;
    }

    loop {
        let mut current_mode = DRM_LSPCON_MODE_INVALID;
        // SAFETY: `adapter` is live for this call per the function's safety contract; `dev` may be null (handled by the callee); `current_mode` is a valid local.
        let ret = unsafe { drm_lspcon_get_mode(dev, adapter, &mut current_mode) };
        if ret != 0 {
            drm_kapi::drm_err!(dev, "can't confirm LSPCON mode change\n");
            return ret;
        }
        if current_mode != mode {
            msleep(10);
            time_out -= 10;
            if time_out <= 0 {
                break;
            }
        } else {
            drm_kapi::drm_dbg!(
                dev,
                "LSPCON mode changed to {}\n",
                if mode == DRM_LSPCON_MODE_LS {
                    "LS"
                } else {
                    "PCON"
                }
            );
            return 0;
        }
    }

    drm_kapi::drm_err!(dev, "LSPCON mode change timed out\n");
    -ETIMEDOUT
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use drm_kapi::i2c::I2cAlgorithm;

    use super::*;

    struct MockDualModeAlgo {
        registers: Mutex<[u8; 256]>,
    }

    impl MockDualModeAlgo {
        const fn new() -> Self {
            Self {
                registers: Mutex::new([0; 256]),
            }
        }

        fn adapter(&self) -> I2cAdapter<'_> {
            I2cAdapter::new("mock-dp++", Some(self))
        }
    }

    impl I2cAlgorithm for MockDualModeAlgo {
        fn functionality(&self) -> u32 {
            drm_kapi::i2c::I2C_FUNC_I2C
        }

        fn master_xfer(&self, _adap: &I2cAdapter<'_>, msgs: &mut [I2cMsg<'_>]) -> i32 {
            let mut regs = self.registers.lock().unwrap();
            let mut msgs_count = 0;
            let mut reg_offset = 0usize;

            for msg in msgs.iter_mut() {
                if msg.addr != DP_DUAL_MODE_SLAVE_ADDRESS {
                    return -EIO;
                }
                if (msg.flags & I2C_M_RD) != 0 {
                    let len = msg.buf.len().min(regs.len() - reg_offset);
                    msg.buf[..len].copy_from_slice(&regs[reg_offset..reg_offset + len]);
                    reg_offset += len;
                } else {
                    if !msg.buf.is_empty() {
                        reg_offset = msg.buf[0] as usize;
                        if msg.buf.len() > 1 {
                            let write_len = (msg.buf.len() - 1).min(regs.len() - reg_offset);
                            regs[reg_offset..reg_offset + write_len]
                                .copy_from_slice(&msg.buf[1..1 + write_len]);
                            // Mirror LSPCON mode change to current mode
                            if reg_offset == DP_DUAL_MODE_LSPCON_MODE_CHANGE as usize {
                                regs[DP_DUAL_MODE_LSPCON_CURRENT_MODE as usize] = regs[reg_offset];
                            }
                        }
                    }
                }
                msgs_count += 1;
            }
            msgs_count
        }
    }

    fn setup_mock_regs(init: impl FnOnce(&mut [u8; 256])) -> MockDualModeAlgo {
        let algo = MockDualModeAlgo::new();
        let mut regs = algo.registers.lock().unwrap();
        *regs = [0; 256];
        init(&mut regs);
        drop(regs);
        algo
    }

    #[test]
    fn test_dual_mode_type_names() {
        // SAFETY: `drm_dp_get_dual_mode_type_name` returns pointers to static NUL-terminated
        // C string literals, which are valid for dereferencing as CStr.
        unsafe {
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(DRM_DP_DUAL_MODE_NONE))
                    .to_bytes(),
                b"none"
            );
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(
                    DRM_DP_DUAL_MODE_TYPE1_DVI
                ))
                .to_bytes(),
                b"type 1 DVI"
            );
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(
                    DRM_DP_DUAL_MODE_TYPE1_HDMI
                ))
                .to_bytes(),
                b"type 1 HDMI"
            );
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(
                    DRM_DP_DUAL_MODE_TYPE2_DVI
                ))
                .to_bytes(),
                b"type 2 DVI"
            );
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(
                    DRM_DP_DUAL_MODE_TYPE2_HDMI
                ))
                .to_bytes(),
                b"type 2 HDMI"
            );
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(DRM_DP_DUAL_MODE_LSPCON))
                    .to_bytes(),
                b"lspcon"
            );
            assert_eq!(
                core::ffi::CStr::from_ptr(drm_dp_get_dual_mode_type_name(999)).to_bytes(),
                b"unknown"
            );
        }
    }

    #[test]
    fn test_dual_mode_detect_type1() {
        let mock_algo = setup_mock_regs(|regs| {
            regs[0..16].copy_from_slice(b"DP-HDMI ADAPTOR\x04");
            regs[DP_DUAL_MODE_ADAPTOR_ID as usize] = 0x00;
        });
        let adapter = mock_algo.adapter();

        // SAFETY: `adapter` is a stack-allocated `I2cAdapter` initialized by `mock_algo.adapter()`.
        // Its pointer `adap_ptr` remains live and aligned across all helper calls in this block;
        // output buffers are valid local references.
        unsafe {
            let adap_ptr = &adapter as *const _ as *mut c_void;
            let kind = drm_dp_dual_mode_detect(ptr::null(), adap_ptr);
            assert_eq!(kind, DRM_DP_DUAL_MODE_TYPE1_HDMI);

            let max_tmds = drm_dp_dual_mode_max_tmds_clock(ptr::null(), kind, adap_ptr);
            assert_eq!(max_tmds, 165000);

            let mut enabled = false;
            let ret = drm_dp_dual_mode_get_tmds_output(ptr::null(), kind, adap_ptr, &mut enabled);
            assert_eq!(ret, 0);
            assert!(enabled);

            let ret = drm_dp_dual_mode_set_tmds_output(ptr::null(), kind, adap_ptr, true);
            assert_eq!(ret, 0);
        }
    }

    #[test]
    fn test_dual_mode_detect_type2_and_tmds_oen() {
        let mock_algo = setup_mock_regs(|regs| {
            regs[0..16].copy_from_slice(b"DP-HDMI ADAPTOR\x04");
            regs[DP_DUAL_MODE_ADAPTOR_ID as usize] =
                DP_DUAL_MODE_TYPE_TYPE2 | DP_DUAL_MODE_REV_TYPE2;
            regs[DP_DUAL_MODE_MAX_TMDS_CLOCK as usize] = 120; // 120 * 2500 = 300000 kHz
            regs[DP_DUAL_MODE_TMDS_OEN as usize] = DP_DUAL_MODE_TMDS_DISABLE;
        });
        let adapter = mock_algo.adapter();

        // SAFETY: `adapter` is a stack-allocated `I2cAdapter` initialized by `mock_algo.adapter()`.
        // Its pointer `adap_ptr` remains live and aligned across all helper calls in this block;
        // `&mut enabled` provides a valid, aligned, exclusive destination buffer.
        unsafe {
            let adap_ptr = &adapter as *const _ as *mut c_void;
            let kind = drm_dp_dual_mode_detect(ptr::null(), adap_ptr);
            assert_eq!(kind, DRM_DP_DUAL_MODE_TYPE2_HDMI);

            let max_tmds = drm_dp_dual_mode_max_tmds_clock(ptr::null(), kind, adap_ptr);
            assert_eq!(max_tmds, 300000);

            let mut enabled = true;
            let ret = drm_dp_dual_mode_get_tmds_output(ptr::null(), kind, adap_ptr, &mut enabled);
            assert_eq!(ret, 0);
            assert!(!enabled);

            let ret = drm_dp_dual_mode_set_tmds_output(ptr::null(), kind, adap_ptr, true);
            assert_eq!(ret, 0);

            let ret = drm_dp_dual_mode_get_tmds_output(ptr::null(), kind, adap_ptr, &mut enabled);
            assert_eq!(ret, 0);
            assert!(enabled);
        }
    }

    #[test]
    fn test_dual_mode_lspcon_mode_switch() {
        let mock_algo = setup_mock_regs(|regs| {
            regs[0..16].copy_from_slice(b"DP-HDMI ADAPTOR\x04");
            regs[DP_DUAL_MODE_ADAPTOR_ID as usize] =
                DP_DUAL_MODE_TYPE_TYPE2 | DP_DUAL_MODE_TYPE_HAS_DPCD;
            regs[DP_DUAL_MODE_LSPCON_CURRENT_MODE as usize] = 0x00; // LS mode
        });
        let adapter = mock_algo.adapter();

        // SAFETY: `adapter` is a stack-allocated `I2cAdapter` initialized by `mock_algo.adapter()`.
        // Its pointer `adap_ptr` remains live and aligned across all helper calls in this block;
        // `&mut mode` provides a valid, aligned destination buffer.
        unsafe {
            let adap_ptr = &adapter as *const _ as *mut c_void;
            let kind = drm_dp_dual_mode_detect(ptr::null(), adap_ptr);
            assert_eq!(kind, DRM_DP_DUAL_MODE_LSPCON);

            let mut mode = DRM_LSPCON_MODE_INVALID;
            let ret = drm_lspcon_get_mode(ptr::null(), adap_ptr, &mut mode);
            assert_eq!(ret, 0);
            assert_eq!(mode, DRM_LSPCON_MODE_LS);

            let ret = drm_lspcon_set_mode(ptr::null(), adap_ptr, DRM_LSPCON_MODE_PCON, 100);
            assert_eq!(ret, 0);

            let ret = drm_lspcon_get_mode(ptr::null(), adap_ptr, &mut mode);
            assert_eq!(ret, 0);
            assert_eq!(mode, DRM_LSPCON_MODE_PCON);

            let ret = drm_lspcon_set_mode(ptr::null(), adap_ptr, DRM_LSPCON_MODE_LS, 100);
            assert_eq!(ret, 0);

            let ret = drm_lspcon_get_mode(ptr::null(), adap_ptr, &mut mode);
            assert_eq!(ret, 0);
            assert_eq!(mode, DRM_LSPCON_MODE_LS);
        }
    }
}
