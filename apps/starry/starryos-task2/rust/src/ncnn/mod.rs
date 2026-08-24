//! Narrow C ABI boundary for the file-backed ncnn YOLO runtime.

use std::ffi::CStr;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Detection {
    pub class_id: u16,
    pub confidence_milli: u16,
    pub center_x_milli: u16,
    pub area_milli: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    NoDetection { infer_us: u64 },
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    Runtime { code: i32, infer_us: u64 },
    #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
    UnsupportedPlatform,
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
unsafe extern "C" {
    fn task3_ncnn_infer(
        param_path: *const std::ffi::c_char,
        model_path: *const std::ffi::c_char,
        input_path: *const std::ffi::c_char,
        detection: *mut Detection,
        infer_us: *mut u64,
    ) -> i32;
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
pub fn infer(
    param_path: &CStr,
    model_path: &CStr,
    input_path: &CStr,
) -> Result<(Detection, u64), Error> {
    let mut detection = Detection {
        class_id: 0,
        confidence_milli: 0,
        center_x_milli: 0,
        area_milli: 0,
    };
    let mut infer_us = 0;
    // The paths are live NUL-terminated strings for the duration of the call,
    // and both output pointers refer to uniquely borrowed initialized values.
    let status = unsafe {
        task3_ncnn_infer(
            param_path.as_ptr(),
            model_path.as_ptr(),
            input_path.as_ptr(),
            &mut detection,
            &mut infer_us,
        )
    };
    match status {
        0 => Ok((detection, infer_us)),
        1 => Err(Error::NoDetection { infer_us }),
        code => Err(Error::Runtime { code, infer_us }),
    }
}

#[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
pub fn infer(
    _param_path: &CStr,
    _model_path: &CStr,
    _input_path: &CStr,
) -> Result<(Detection, u64), Error> {
    Err(Error::UnsupportedPlatform)
}
