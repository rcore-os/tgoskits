.section .text

.macro SAVE_GUEST_REGS
    st.d    $r0, $sp, 0
    st.d    $r1, $sp, 8
    st.d    $r2, $sp, 16
    st.d    $r3, $sp, 24
    st.d    $r4, $sp, 32
    st.d    $r5, $sp, 40
    st.d    $r6, $sp, 48
    st.d    $r7, $sp, 56
    st.d    $r8, $sp, 64
    st.d    $r9, $sp, 72
    st.d    $r10, $sp, 80
    st.d    $r11, $sp, 88
    st.d    $r12, $sp, 96
    st.d    $r13, $sp, 104
    st.d    $r14, $sp, 112
    st.d    $r15, $sp, 120
    st.d    $r16, $sp, 128
    st.d    $r17, $sp, 136
    st.d    $r18, $sp, 144
    st.d    $r19, $sp, 152
    st.d    $r20, $sp, 160
    st.d    $r21, $sp, 168
    st.d    $r22, $sp, 176
    st.d    $r23, $sp, 184
    st.d    $r24, $sp, 192
    st.d    $r25, $sp, 200
    st.d    $r26, $sp, 208
    st.d    $r27, $sp, 216
    st.d    $r28, $sp, 224
    st.d    $r29, $sp, 232
    st.d    $r30, $sp, 240
    st.d    $r31, $sp, 248

    gcsrrd  $t0, 0x6
    st.d    $t0, $sp, 256
    gcsrrd  $t0, 0x0
    st.d    $t0, $sp, 264
    gcsrrd  $t0, 0x1
    st.d    $t0, $sp, 272
    gcsrrd  $t0, 0x5
    st.d    $t0, $sp, 280
.endm

.macro RESTORE_GUEST_REGS
    ld.d    $t0, $sp, 256
    gcsrwr  $t0, 0x6
    ld.d    $t0, $sp, 264
    gcsrwr  $t0, 0x0
    ld.d    $t0, $sp, 272
    gcsrwr  $t0, 0x1
    ld.d    $t0, $sp, 280
    gcsrwr  $t0, 0x5

    ld.d    $r1, $sp, 8
    ld.d    $r2, $sp, 16
    ld.d    $r3, $sp, 24
    ld.d    $r4, $sp, 32
    ld.d    $r5, $sp, 40
    ld.d    $r6, $sp, 48
    ld.d    $r7, $sp, 56
    ld.d    $r8, $sp, 64
    ld.d    $r9, $sp, 72
    ld.d    $r10, $sp, 80
    ld.d    $r11, $sp, 88
    ld.d    $r12, $sp, 96
    ld.d    $r13, $sp, 104
    ld.d    $r14, $sp, 112
    ld.d    $r15, $sp, 120
    ld.d    $r16, $sp, 128
    ld.d    $r17, $sp, 136
    ld.d    $r18, $sp, 144
    ld.d    $r19, $sp, 152
    ld.d    $r20, $sp, 160
    ld.d    $r21, $sp, 168
    ld.d    $r22, $sp, 176
    ld.d    $r23, $sp, 184
    ld.d    $r24, $sp, 192
    ld.d    $r25, $sp, 200
    ld.d    $r26, $sp, 208
    ld.d    $r27, $sp, 216
    ld.d    $r28, $sp, 224
    ld.d    $r29, $sp, 232
    ld.d    $r30, $sp, 240
    ld.d    $r31, $sp, 248
    ld.d    $r0, $sp, 0
.endm

.global _run_guest
_run_guest:
    move    $t1, $ra
    csrwr   $a0, 0x33
    move    $sp, $a0
    RESTORE_GUEST_REGS
    ertn
    jr      $t1

.global _guest_exit
_guest_exit:
    b       vmexit_trampoline

.section .vectors, "ax"
.align 12
.global _exception_vectors
_exception_vectors:
    .org 0x0
    b       _irq_handler_vector
    .org 0x80
    b       _sync_handler_vector
    .org 0x100
    b       _sync_handler_vector
    .org 0x180
    b       _sync_handler_vector
    .org 0x200
    b       _sync_handler_vector
    .org 0x280
    b       _sync_handler_vector
    .org 0x400
    b       _sync_handler_vector
    .org 0x480
    b       _sync_handler_vector
    .org 0x500
    b       _sync_handler_vector
    .org 0x580
    b       _sync_handler_vector
    .org 0x600
    b       _sync_handler_vector
    .org 0x680
    b       _sync_handler_vector
    .org 0x700
    b       _sync_handler_vector
    .org 0x780
    b       _sync_handler_vector
    .org 0x800
    b       _sync_handler_vector
    .org 0x880
    b       _sync_handler_vector
    .org 0x900
    b       _sync_handler_vector
    .org 0x980
    b       _sync_handler_vector
    .org 0xa00
    b       _sync_handler_vector
    .org 0xa80
    b       _sync_handler_vector
    .org 0xb00
    b       _hvc_handler_vector
    .org 0xb80
    b       _sync_handler_vector
    .org 0xc00
    b       _sync_handler_vector
    .org 0xc80
    b       _sync_handler_vector
    .org 0xd00
    b       _sync_handler_vector
    .org 0xd80
    b       _sync_handler_vector

_irq_handler_vector:
    csrrd   $sp, 0x33
    SAVE_GUEST_REGS
    li.d    $a0, 1
    b       _guest_exit

_sync_handler_vector:
    csrrd   $sp, 0x33
    SAVE_GUEST_REGS
    li.d    $a0, 0
    b       _guest_exit

_hvc_handler_vector:
    csrrd   $sp, 0x33
    SAVE_GUEST_REGS
    li.d    $a0, 0
    b       _guest_exit
