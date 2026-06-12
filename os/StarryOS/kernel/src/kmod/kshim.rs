//! Compatibility symbols for Linux C modules.

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use kmod::capi_fn;

    #[capi_fn]
    #[unsafe(naked)]
    unsafe extern "C" fn __x86_return_thunk() {
        core::arch::naked_asm!("ret");
    }
}
