// Boot vectors share the CPU integer save/restore definition.
.section .text.__ax_cpu_boot_vector, "ax"
.balign 0x10000
.global __ax_cpu_boot_vector
__ax_cpu_boot_vector:
.rept 128
    b .Lax_cpu_boot_exception
    .balign 0x200
.endr
.Lax_cpu_boot_exception:
    addi.d $sp, $sp, -{frame_size}
    st.d $zero, $sp, 0
    PUSH_POP_GENERAL_REGS STD
    addi.d $t0, $sp, {frame_size}
    st.d $t0, $sp, {sp_offset}
    csrrd $t0, 0x1
    st.d $t0, $sp, {prmd_offset}
    csrrd $t0, 0x6
    st.d $t0, $sp, {era_offset}
    move $a0, $sp
    bl {dispatch}
    ld.d $t0, $sp, {era_offset}
    csrwr $t0, 0x6
    ld.d $t0, $sp, {prmd_offset}
    csrwr $t0, 0x1
    PUSH_POP_GENERAL_REGS LDD
    ld.d $sp, $sp, {sp_offset}
    ertn
