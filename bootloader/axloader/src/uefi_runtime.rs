/// Returns the length of a null-terminated UEFI wide string.
///
/// LLVM 23 recognizes UTF-16 length loops as calls to `wcslen`. The UEFI
/// target does not link a C runtime, so AxLoader supplies the symbol until the
/// implementation added to `compiler-builtins` reaches the pinned toolchain.
///
/// # Safety
///
/// `string` must point to a valid sequence of `u16` values terminated by zero.
#[inline(never)]
#[unsafe(no_mangle)]
unsafe extern "C" fn wcslen(string: *const u16) -> usize {
    let mut current = string;
    let mut len = 0;

    // SAFETY: The caller guarantees that `current` starts within a valid
    // zero-terminated sequence and remains within it through each increment.
    while unsafe { *current } != 0 {
        len += 1;
        // SAFETY: The terminator has not been reached, so the caller's validity
        // guarantee covers the next element in the sequence.
        current = unsafe { current.add(1) };
    }

    len
}
