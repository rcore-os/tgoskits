.section .text

.macro SAVE_GUEST_REGS
    st.d    $r0,  $sp, 0
    st.d    $r1,  $sp, 8
    st.d    $r2,  $sp, 16
    csrrd   $t0, 0x502
    st.d    $t0, $sp, 24
    st.d    $r4,  $sp, 32
    st.d    $r5,  $sp, 40
    st.d    $r6,  $sp, 48
    st.d    $r7,  $sp, 56
    st.d    $r8,  $sp, 64
    st.d    $r9,  $sp, 72
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

    csrrd   $t0, 0x6
    st.d    $t0, $sp, 256
    gcsrrd  $t0, 0x0
    st.d    $t0, $sp, 264
    gcsrrd  $t0, 0x1
    st.d    $t0, $sp, 272
    gcsrrd  $t0, 0x2
    st.d    $t0, $sp, 280
    gcsrrd  $t0, 0x3
    st.d    $t0, $sp, 288
    gcsrrd  $t0, 0x4
    st.d    $t0, $sp, 296
    gcsrrd  $t0, 0x5
    st.d    $t0, $sp, 304
    gcsrrd  $t0, 0x6
    st.d    $t0, $sp, 312
    gcsrrd  $t0, 0x7
    st.d    $t0, $sp, 320
    gcsrrd  $t0, 0x8
    st.d    $t0, $sp, 328
    gcsrrd  $t0, 0xc
    st.d    $t0, $sp, 336
    gcsrrd  $t0, 0x10
    st.d    $t0, $sp, 344
    gcsrrd  $t0, 0x11
    st.d    $t0, $sp, 352
    gcsrrd  $t0, 0x12
    st.d    $t0, $sp, 360
    gcsrrd  $t0, 0x13
    st.d    $t0, $sp, 368
    gcsrrd  $t0, 0x18
    st.d    $t0, $sp, 376
    gcsrrd  $t0, 0x19
    st.d    $t0, $sp, 384
    gcsrrd  $t0, 0x1a
    st.d    $t0, $sp, 392
    gcsrrd  $t0, 0x1b
    st.d    $t0, $sp, 400
    gcsrrd  $t0, 0x1c
    st.d    $t0, $sp, 408
    gcsrrd  $t0, 0x1d
    st.d    $t0, $sp, 416
    gcsrrd  $t0, 0x1e
    st.d    $t0, $sp, 424
    gcsrrd  $t0, 0x1f
    st.d    $t0, $sp, 432
    gcsrrd  $t0, 0x20
    st.d    $t0, $sp, 440
    gcsrrd  $t0, 0x21
    st.d    $t0, $sp, 448
    gcsrrd  $t0, 0x22
    st.d    $t0, $sp, 456
    gcsrrd  $t0, 0x23
    st.d    $t0, $sp, 464
    gcsrrd  $t0, 0x30
    st.d    $t0, $sp, 472
    gcsrrd  $t0, 0x31
    st.d    $t0, $sp, 480
    gcsrrd  $t0, 0x32
    st.d    $t0, $sp, 488
    gcsrrd  $t0, 0x33
    st.d    $t0, $sp, 496
    gcsrrd  $t0, 0x34
    st.d    $t0, $sp, 504
    gcsrrd  $t0, 0x35
    st.d    $t0, $sp, 512
    gcsrrd  $t0, 0x36
    st.d    $t0, $sp, 520
    gcsrrd  $t0, 0x37
    st.d    $t0, $sp, 528
    gcsrrd  $t0, 0x38
    st.d    $t0, $sp, 536
    gcsrrd  $t0, 0x39
    st.d    $t0, $sp, 544
    gcsrrd  $t0, 0x3a
    st.d    $t0, $sp, 552
    gcsrrd  $t0, 0x3b
    st.d    $t0, $sp, 560
    gcsrrd  $t0, 0x3c
    st.d    $t0, $sp, 568
    gcsrrd  $t0, 0x3d
    st.d    $t0, $sp, 576
    gcsrrd  $t0, 0x3e
    st.d    $t0, $sp, 584
    gcsrrd  $t0, 0x3f
    st.d    $t0, $sp, 592
    gcsrrd  $t0, 0x40
    st.d    $t0, $sp, 600
    gcsrrd  $t0, 0x41
    st.d    $t0, $sp, 608
    gcsrrd  $t0, 0x42
    st.d    $t0, $sp, 616
    gcsrrd  $t0, 0x43
    st.d    $t0, $sp, 624
    gcsrrd  $t0, 0x44
    st.d    $t0, $sp, 632
    gcsrrd  $t0, 0x60
    st.d    $t0, $sp, 640
    gcsrrd  $t0, 0x88
    st.d    $t0, $sp, 648
    gcsrrd  $t0, 0x89
    st.d    $t0, $sp, 656
    gcsrrd  $t0, 0x8a
    st.d    $t0, $sp, 664
    gcsrrd  $t0, 0x8b
    st.d    $t0, $sp, 672
    gcsrrd  $t0, 0x8c
    st.d    $t0, $sp, 680
    gcsrrd  $t0, 0x8d
    st.d    $t0, $sp, 688
    gcsrrd  $t0, 0x8e
    st.d    $t0, $sp, 696
    gcsrrd  $t0, 0x8f
    st.d    $t0, $sp, 704
    gcsrrd  $t0, 0x180
    st.d    $t0, $sp, 712
    gcsrrd  $t0, 0x181
    st.d    $t0, $sp, 720
    gcsrrd  $t0, 0x182
    st.d    $t0, $sp, 728
    gcsrrd  $t0, 0x183
    st.d    $t0, $sp, 736

    csrrd   $t0, 0x5
    st.d    $t0, $sp, 744
    csrrd   $t0, 0x6
    st.d    $t0, $sp, 752
    csrrd   $t0, 0x7
    st.d    $t0, $sp, 760
    csrrd   $t0, 0x8
    st.d    $t0, $sp, 768
    csrrd   $t0, 0x89
    st.d    $t0, $sp, 776
    csrrd   $t0, 0x8a
    st.d    $t0, $sp, 784
.endm

.macro RESTORE_GUEST_REGS
    ld.d    $t0, $sp, 24
    csrwr   $t0, 0x502

    ld.d    $t0, $sp, 256
    csrwr   $t0, 0x6
    ld.d    $t0, $sp, 264
    gcsrwr  $t0, 0x0
    ld.d    $t0, $sp, 272
    gcsrwr  $t0, 0x1
    ld.d    $t0, $sp, 280
    gcsrwr  $t0, 0x2
    ld.d    $t0, $sp, 288
    gcsrwr  $t0, 0x3
    ld.d    $t0, $sp, 296
    gcsrwr  $t0, 0x4
    ld.d    $t0, $sp, 304
    gcsrwr  $t0, 0x5
    ld.d    $t0, $sp, 312
    gcsrwr  $t0, 0x6
    ld.d    $t0, $sp, 320
    gcsrwr  $t0, 0x7
    ld.d    $t0, $sp, 328
    gcsrwr  $t0, 0x8
    ld.d    $t0, $sp, 336
    gcsrwr  $t0, 0xc
    ld.d    $t0, $sp, 344
    gcsrwr  $t0, 0x10
    ld.d    $t0, $sp, 352
    gcsrwr  $t0, 0x11
    ld.d    $t0, $sp, 360
    gcsrwr  $t0, 0x12
    ld.d    $t0, $sp, 368
    gcsrwr  $t0, 0x13
    ld.d    $t0, $sp, 376
    gcsrwr  $t0, 0x18
    ld.d    $t0, $sp, 384
    gcsrwr  $t0, 0x19
    ld.d    $t0, $sp, 392
    gcsrwr  $t0, 0x1a
    ld.d    $t0, $sp, 400
    gcsrwr  $t0, 0x1b
    ld.d    $t0, $sp, 408
    gcsrwr  $t0, 0x1c
    ld.d    $t0, $sp, 416
    gcsrwr  $t0, 0x1d
    ld.d    $t0, $sp, 424
    gcsrwr  $t0, 0x1e
    ld.d    $t0, $sp, 432
    gcsrwr  $t0, 0x1f
    ld.d    $t0, $sp, 440
    gcsrwr  $t0, 0x20
    ld.d    $t0, $sp, 448
    gcsrwr  $t0, 0x21
    ld.d    $t0, $sp, 456
    gcsrwr  $t0, 0x22
    ld.d    $t0, $sp, 464
    gcsrwr  $t0, 0x23
    ld.d    $t0, $sp, 472
    gcsrwr  $t0, 0x30
    ld.d    $t0, $sp, 480
    gcsrwr  $t0, 0x31
    ld.d    $t0, $sp, 488
    gcsrwr  $t0, 0x32
    ld.d    $t0, $sp, 496
    gcsrwr  $t0, 0x33
    ld.d    $t0, $sp, 504
    gcsrwr  $t0, 0x34
    ld.d    $t0, $sp, 512
    gcsrwr  $t0, 0x35
    ld.d    $t0, $sp, 520
    gcsrwr  $t0, 0x36
    ld.d    $t0, $sp, 528
    gcsrwr  $t0, 0x37
    ld.d    $t0, $sp, 536
    gcsrwr  $t0, 0x38
    ld.d    $t0, $sp, 544
    gcsrwr  $t0, 0x39
    ld.d    $t0, $sp, 552
    gcsrwr  $t0, 0x3a
    ld.d    $t0, $sp, 560
    gcsrwr  $t0, 0x3b
    ld.d    $t0, $sp, 568
    gcsrwr  $t0, 0x3c
    ld.d    $t0, $sp, 576
    gcsrwr  $t0, 0x3d
    ld.d    $t0, $sp, 584
    gcsrwr  $t0, 0x3e
    ld.d    $t0, $sp, 592
    gcsrwr  $t0, 0x3f
    ld.d    $t0, $sp, 600
    gcsrwr  $t0, 0x40
    ld.d    $t0, $sp, 608
    gcsrwr  $t0, 0x41
    ld.d    $t0, $sp, 616
    gcsrwr  $t0, 0x42
    ld.d    $t0, $sp, 624
    gcsrwr  $t0, 0x43
    ld.d    $t0, $sp, 632
    gcsrwr  $t0, 0x44
    ld.d    $t0, $sp, 640
    gcsrwr  $t0, 0x60
    ld.d    $t0, $sp, 648
    gcsrwr  $t0, 0x88
    ld.d    $t0, $sp, 656
    gcsrwr  $t0, 0x89
    ld.d    $t0, $sp, 664
    gcsrwr  $t0, 0x8a
    ld.d    $t0, $sp, 672
    gcsrwr  $t0, 0x8b
    ld.d    $t0, $sp, 680
    gcsrwr  $t0, 0x8c
    ld.d    $t0, $sp, 688
    gcsrwr  $t0, 0x8d
    ld.d    $t0, $sp, 696
    gcsrwr  $t0, 0x8e
    ld.d    $t0, $sp, 704
    gcsrwr  $t0, 0x8f
    ld.d    $t0, $sp, 712
    gcsrwr  $t0, 0x180
    ld.d    $t0, $sp, 720
    gcsrwr  $t0, 0x181
    ld.d    $t0, $sp, 728
    gcsrwr  $t0, 0x182
    ld.d    $t0, $sp, 736
    gcsrwr  $t0, 0x183

    ld.d    $r1,  $sp, 8
    ld.d    $r2,  $sp, 16
    ld.d    $r4,  $sp, 32
    ld.d    $r5,  $sp, 40
    ld.d    $r6,  $sp, 48
    ld.d    $r7,  $sp, 56
    ld.d    $r8,  $sp, 64
    ld.d    $r9,  $sp, 72
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
    ld.d    $r0,  $sp, 0
    csrwr   $sp, 0x502
.endm

.global _run_guest
_run_guest:
    move    $t1, $ra
    addi.d  $t0, $a0, 792
    csrwr   $t0, 0x33
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
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    li.d    $a0, 1
    b       _guest_exit

_sync_handler_vector:
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    li.d    $a0, 0
    b       _guest_exit

_hvc_handler_vector:
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    li.d    $a0, 0
    b       _guest_exit
