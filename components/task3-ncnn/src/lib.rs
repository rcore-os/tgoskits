//! A small C ABI wrapper around ncnn for the Linux Guest.
//!
//! The wrapper deliberately owns only file-backed model/input loading and
//! tensor extraction.  Detection validation and control mapping remain in
//! `task3_model::perception`, so ncnn output cannot bypass the safety contract.

#![no_std]

use core::ffi::c_char;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Detection {
    pub class_id: u16,
    pub confidence_milli: u16,
    pub center_x_milli: u16,
    pub area_milli: u16,
}

unsafe extern "C" {
    fn task3_ncnn_infer(
        param_path: *const i8,
        model_path: *const i8,
        input_path: *const i8,
        detection: *mut Detection,
        infer_us: *mut u64,
    ) -> i32;
}

/// Runs one file-backed ncnn inference and returns the decoded detection.
///
/// # Safety
///
/// Each path must be a non-null pointer to a valid NUL-terminated string that
/// remains readable for the duration of the call.
pub unsafe fn infer(
    param_path: *const c_char,
    model_path: *const c_char,
    input_path: *const c_char,
) -> Result<(Detection, u64), (i32, u64)> {
    let mut detection = Detection {
        class_id: 0,
        confidence_milli: 0,
        center_x_milli: 0,
        area_milli: 0,
    };
    let mut infer_us = 0;
    let status = unsafe {
        task3_ncnn_infer(
            param_path.cast(),
            model_path.cast(),
            input_path.cast(),
            &mut detection,
            &mut infer_us,
        )
    };
    if status == 0 {
        Ok((detection, infer_us))
    } else {
        Err((status, infer_us))
    }
}
