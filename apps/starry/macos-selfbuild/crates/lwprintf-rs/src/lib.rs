//! LwPRINTF Rust bindings and wrappers.
//!
#![no_std]
#![feature(c_variadic)]
#![deny(missing_docs)]
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    clashing_extern_declarations
)]

mod bindings {
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/lwprintf.rs"));
}
use core::ptr::null_mut;

use bindings::lwprintf_t;

/// Maximum size value for buffers.
pub const SIZE_MAX: i32 = bindings::SIZE_MAX as _;

/// Trait for custom output handling.
pub trait CustomOutPut {
    /// Output a single character.
    fn putch(ch: i32) -> i32;
}

/// LwPRINTF object with custom output handler.
pub struct LwprintfObj<T: CustomOutPut> {
    obj: lwprintf_t,
    _phantom: core::marker::PhantomData<T>,
}

impl<T: CustomOutPut> Default for LwprintfObj<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: CustomOutPut> LwprintfObj<T> {
    /// Create a new uninitialized LwPRINTF object.
    pub fn new() -> Self {
        let obj = lwprintf_t {
            out_fn: None,
            arg: core::ptr::null_mut(),
        };

        Self {
            obj,
            _phantom: core::marker::PhantomData,
        }
    }

    /// Get a mutable reference to the underlying lwprintf_t object.
    ///
    /// This allows calling sys functions directly with the object.
    pub fn as_mut_ptr(&mut self) -> *mut lwprintf_t {
        &mut self.obj
    }
}

extern "C" fn out_fn<T: CustomOutPut>(
    ch: core::ffi::c_int,
    _lwobj: *mut lwprintf_t,
) -> core::ffi::c_int {
    T::putch(ch)
}

/// Initialize lwprintf object with custom output function.
pub fn lwprintf_init_ex<T: CustomOutPut>(lwobj: &mut LwprintfObj<T>) -> u8 {
    unsafe { bindings::lwprintf_init_ex(lwobj.as_mut_ptr(), Some(out_fn::<T>)) }
}

/// Initialize default lwprintf instance.
pub fn lwprintf_init<T: CustomOutPut>() -> u8 {
    unsafe { bindings::lwprintf_init_ex(null_mut(), Some(out_fn::<T>)) }
}

mod sys_inner {
    use core::ffi::VaList;

    use crate::bindings::lwprintf_t;

    unsafe extern "C" {
        /// Print formatted data from variable argument list to the output.
        /// # Arguments
        /// * `lwobj` - LwPRINTF instance. Set to NULL to use default instance.
        /// * `format` - C string that contains the text to be written to output.
        /// * `arg` - A value identifying a variable arguments list initialized with va_start. va_list is a special type defined in cstdarg.
        pub fn lwprintf_vprintf_ex(
            lwobj: *mut lwprintf_t,
            format: *const core::ffi::c_char,
            arg: VaList,
        ) -> core::ffi::c_int;

        /// Write formatted data from variable argument list to sized buffer.
        /// # Arguments
        /// * `lwobj` - LwPRINTF instance. Set to NULL to use default instance.
        /// * `s` - Pointer to a buffer where the resulting C-string is stored. The buffer should have a size of at least n characters.
        /// * `n` - Maximum number of bytes to be used in the buffer. The generated string has a length of at most n - 1, leaving space for the additional terminating null character.
        /// * `format` - C string that contains the text to be written to output.
        /// * `arg` - A value identifying a variable arguments list initialized with va_start. va_list is a special type defined in cstdarg.
        pub fn lwprintf_vsnprintf_ex(
            lwobj: *mut lwprintf_t,
            s: *mut core::ffi::c_char,
            n: usize,
            format: *const core::ffi::c_char,
            arg: VaList,
        ) -> core::ffi::c_int;

        /// Print formatted data to the output.
        /// # Arguments
        /// * `lwobj` - LwPRINTF instance. Set to NULL to use default instance.
        /// * `format` - C string that contains the text to be written to output.
        /// * `...` -  Optional arguments for format string.
        pub fn lwprintf_printf_ex(
            lwobj: *mut lwprintf_t,
            format: *const core::ffi::c_char,
            ...
        ) -> core::ffi::c_int;

        /// Write formatted data from variable argument list to sized buffer.
        /// # Arguments
        /// * `lwobj` - LwPRINTF instance. Set to NULL to use default instance.
        /// * `s` - Pointer to a buffer where the resulting C-string is stored. The buffer should have a size of at least n characters.
        /// * `n` - Maximum number of bytes to be used in the buffer. The generated string has a length of at most n - 1, leaving space for the additional terminating null character.
        /// * `format` - C string that contains a format string that follows the same specifications as format in printf.
        /// * `...` -  Optional arguments for format string.
        /// # Returns
        /// The number of characters that would have been written if n had been sufficiently large, not counting the terminating null character.
        pub fn lwprintf_snprintf_ex(
            lwobj: *mut lwprintf_t,
            s: *mut ::core::ffi::c_char,
            n: usize,
            format: *const ::core::ffi::c_char,
            ...
        ) -> ::core::ffi::c_int;
    }
}

pub use sys_inner::lwprintf_printf_ex;
pub use sys_inner::lwprintf_snprintf_ex;
pub use sys_inner::lwprintf_vprintf_ex;
pub use sys_inner::lwprintf_vsnprintf_ex;

/// Print formatted data from variable argument list to the output.
///
/// **WARNING**: This function is an wrapper for [lwprintf_vprintf_ex] and uses Rust's
/// variadic arguments feature. If you plan to call this function from C code or need
/// precise control over the `va_list`, use [lwprintf_vprintf_ex] directly.
///
/// # Arguments
/// * `args` - Additional arguments specifying data to print.
/// * other arguments are the same as [lwprintf_vprintf_ex].
///
/// # Safety
/// This function is unsafe because it uses C-style variadic arguments.
pub unsafe extern "C" fn lwprintf_vprintf_ex_rust(
    lwobj: *mut lwprintf_t,
    fmt: *const core::ffi::c_char,
    args: ...
) -> core::ffi::c_int {
    unsafe { sys_inner::lwprintf_vprintf_ex(lwobj, fmt, args) }
}

/// Write formatted data from variable argument list to sized buffer.
///
/// **WARNING**: This function is an wrapper for [lwprintf_vsnprintf_ex] and uses Rust's
/// variadic arguments feature. If you plan to call this function from C code or need
/// precise control over the `va_list`, use [lwprintf_vsnprintf_ex] directly
/// # Arguments
/// * `args` - Additional arguments specifying data to print.
/// * other arguments are the same as [lwprintf_vsnprintf_ex].
///
/// # Safety
/// This function is unsafe because it uses C-style variadic arguments.
pub unsafe extern "C" fn lwprintf_vsnprintf_ex_rust(
    lwobj: *mut lwprintf_t,
    s: *mut core::ffi::c_char,
    n: usize,
    fmt: *const core::ffi::c_char,
    args: ...
) -> core::ffi::c_int {
    unsafe { sys_inner::lwprintf_vsnprintf_ex(lwobj, s, n, fmt, args) }
}

/// Write formatted data from variable argument list to sized buffer.
/// This macro uses [lwprintf_snprintf_ex] internally with `n` set to `SIZE_MAX`.
///
/// **WARNING:** User is responsible for ensuring that the buffer is large enough to hold the formatted string.
#[macro_export]
macro_rules! lwprintf_sprintf_ex {
    ( $lwobj:expr, $buf:expr, $format:expr, $( $args:expr ),* ) => {
        unsafe {
            $crate::lwprintf_snprintf_ex(
                $lwobj,
                $buf,
                $crate::SIZE_MAX as usize,
                $format,
                $( $args ),*
            )
        }
    };
}

/// Print formatted data from variable argument list to the output with default LwPRINTF instance.
/// This macro uses [lwprintf_vprintf_ex] internally with `lwobj` set to NULL.
#[macro_export]
macro_rules! lwprintf_vprintf {
    ( $format:expr, $arg: expr ) => {
        unsafe { $crate::lwprintf_vprintf_ex(core::ptr::null_mut(), $format, $arg) }
    };
}

/// Print formatted data to the output with default LwPRINTF instance.
///
/// This macro uses [lwprintf_printf_ex] internally with `lwobj` set to NULL.
#[macro_export]
macro_rules! lwprintf_printf {
    ($format:expr, $( $args:expr ),* ) => {
        unsafe {
            $crate::lwprintf_printf_ex(
                core::ptr::null_mut(),
                $format,
                $( $args ),*
            )
        }
    };
}

/// Write formatted data from variable argument list to sized buffer with default LwPRINTF instance.
///
/// This macro uses [lwprintf_vsnprintf_ex] internally with `lwobj` set to NULL.
#[macro_export]
macro_rules! lwprintf_vsnprintf {
    ( $buf:expr, $n:expr, $format:expr, $arg: expr ) => {
        unsafe { $crate::lwprintf_vsnprintf_ex(core::ptr::null_mut(), $buf, $n, $format, $arg) }
    };
}

/// Write formatted data to sized buffer with default LwPRINTF instance.
///
/// This macro uses [lwprintf_snprintf_ex] internally with `lwobj` set to NULL.
#[macro_export]
macro_rules! lwprintf_snprintf {
    ( $buf:expr, $n:expr, $format:expr, $( $args:expr ),* ) => {
        unsafe {
            $crate::lwprintf_snprintf_ex(
                core::ptr::null_mut(),
                $buf,
                $n,
                $format,
                $( $args ),*
            )
        }
    };
}

/// Write formatted data from variable argument list to sized buffer with default LwPRINTF instance.
///
/// This macro uses [lwprintf_snprintf_ex] internally with `lwobj` set to NULL and `n` set to `SIZE_MAX`.
#[macro_export]
macro_rules! lwprintf_sprintf {
    ($buf:expr, $format:expr, $( $args:expr ),* ) => {
        unsafe {
            $crate::lwprintf_sprintf_ex!(core::ptr::null_mut(), $buf, $format, $( $args ),* )
        }
    };
}
