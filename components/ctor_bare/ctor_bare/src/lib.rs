#![no_std]
#![doc = include_str!("../README.md")]

pub use ctor_bare_macros::register_ctor;

/// Placeholder for the `.init_array` section, so that
/// the `__init_array_start` and `__init_array_end` symbols can be generated.
#[unsafe(link_section = ".init_array")]
#[used]
static _SECTION_PLACE_HOLDER: [u8; 0] = [];

unsafe extern "C" {
    fn __init_array_start();
    fn __init_array_end();
}

/// Invoke all constructor functions registered by the `register_ctor` attribute.
///
/// # Notes
/// Caller should ensure that the `.init_array` section will not be disturbed by other sections.
pub fn call_ctors() {
    for ctor_ptr in (__init_array_start as usize..__init_array_end as usize)
        .step_by(core::mem::size_of::<*const core::ffi::c_void>())
    {
        unsafe {
            core::mem::transmute::<*const core::ffi::c_void, fn()>(
                *(ctor_ptr as *const *const core::ffi::c_void),
            )();
        }
    }
}
