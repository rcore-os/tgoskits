//! Runtime support for compiler-inserted stack protector checks.

const STACK_CHK_GUARD: usize = 0x57AC_CE11_5A7A_CE11usize;

#[unsafe(no_mangle)]
#[used]
pub static __stack_chk_guard: usize = STACK_CHK_GUARD;

#[unsafe(no_mangle)]
pub extern "C" fn __stack_chk_fail() -> ! {
    panic!("stack-protector: kernel stack is corrupted")
}
