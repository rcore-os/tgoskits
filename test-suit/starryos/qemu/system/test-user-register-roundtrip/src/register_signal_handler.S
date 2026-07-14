/*
 * The handler must observe the interrupted thread pointer before any compiler
 * prologue or libc TLS access. The raw tgkill probe keeps the result pointer in
 * s2, which the kernel signal frame must preserve as user-owned state.
 */

    .text
    .align  2
    .global register_signal_handler
    .type   register_signal_handler, @function
register_signal_handler:
#if defined(__riscv) && __riscv_xlen == 64
    sd      gp, 0(s2)
    sd      tp, 8(s2)
    ret
#elif defined(__loongarch__) || defined(__loongarch64)
    st.d    $r21, $s2, 0
    st.d    $tp, $s2, 8
    jr      $ra
#else
#error "register signal probe requires riscv64 or loongarch64"
#endif
    .size   register_signal_handler, .-register_signal_handler

    .section .note.GNU-stack,"",@progbits
