.section .text

.equ CSR_GSTAT,        0x50
.equ CSR_GTLBC,        0x15
.equ CSR_CRMD,         0x0
.equ CSR_PRMD,         0x1
.equ CSR_ECFG,         0x4
.equ CSR_BADI,         0x8
.equ CSR_SAVE0,        0x30
.equ CSR_SAVE1,        0x31
.equ CSR_PGD,          0x1b
.equ CSR_TLBRERA,      0x8a
.equ CSR_TLBRSAVE,     0x8b
.equ CSR_TLBRELO0,     0x8c
.equ CSR_TLBRELO1,     0x8d
.equ CSR_TLBREHI,      0x8e
.equ CSR_TLBRPRMD,     0x8f
.equ CSR_GSTAT_PVM,    0x2
.equ CSR_PRMD_PIE,     0x4
.equ CSR_TLBRPRMD_PGM, 0x8
.equ CSR_GTLBC_TGID,   0xff0000
.equ CSR_GTLBC_TGID0,  0x0
.equ CSR_TIMER_IE,     0x800
.equ CSR_CRMD_IE,      0x4

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
    csrrd   $t0, CSR_SAVE0
    st.d    $t0, $sp, 96
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
    /*
     * Keep the software timer's logical TVAL in the Rust context. QEMU-LVZ
     * reuses GCSR_TVAL as a pause/resume remaining-ticks value while the guest
     * is stopped, so snapshotting it here pollutes the emulated guest timer.
     */
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
    csrrd   $t0, CSR_BADI
    st.d    $t0, $sp, 768
    csrrd   $t0, 0x89
    st.d    $t0, $sp, 776
    csrrd   $t0, CSR_TLBRERA
    st.d    $t0, $sp, 784
.endm

.macro ENTER_HOST_MODE
    li.d    $t0, CSR_GSTAT_PVM
    csrxchg $zero, $t0, CSR_GSTAT
    csrrd   $t1, CSR_TLBRPRMD
    li.d    $t0, -9
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRPRMD
    csrrd   $t1, CSR_GTLBC
    li.d    $t0, CSR_GTLBC_TGID
    andn    $t1, $t1, $t0
    csrwr   $t1, CSR_GTLBC
.endm

.macro DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $t1, CSR_SAVE1
    csrrd   $t0, CSR_ECFG
    li.d    $t1, CSR_TIMER_IE
    andn    $t0, $t0, $t1
    csrwr   $t0, CSR_ECFG
    csrrd   $t1, CSR_SAVE1
.endm

.macro PREPARE_GUEST_ENTRY
    li.d    $t0, CSR_CRMD_IE
    csrxchg $zero, $t0, CSR_CRMD

    li.d    $t0, CSR_PRMD_PIE
    csrwr   $t0, CSR_PRMD

    csrrd   $t1, CSR_TLBRERA
    li.d    $t0, -2
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRERA

    csrrd   $t1, CSR_GTLBC
    li.d    $t0, CSR_GTLBC_TGID
    andn    $t1, $t1, $t0
    ori     $t1, $t1, CSR_GTLBC_TGID0
    csrwr   $t1, CSR_GTLBC

    li.d    $t0, CSR_GSTAT_PVM
    csrxchg $t0, $t0, CSR_GSTAT

    csrrd   $t0, CSR_ECFG
    ori     $t0, $t0, CSR_TIMER_IE
    csrwr   $t0, CSR_ECFG
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
    gcsrrd  $t1, 0x5
    li.d    $t2, 0x0fff
    and     $t1, $t1, $t2
    ld.d    $t0, $sp, 304
    or      $t0, $t0, $t1
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
    /*
     * QEMU-LVZ stores GCSR_TVAL as remaining ticks while guest mode is
     * paused.  Rewriting it on every VM entry re-bases the relative guest
     * timer and can keep pushing one-shot deadlines into the future.
     */
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
    ld.d    $t0, $sp, 256
    csrwr   $t0, 0x6
    csrrd   $t1, CSR_TLBRERA
    li.d    $t0, -2
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRERA
    csrrd   $t1, CSR_TLBRPRMD
    li.d    $t0, -9
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRPRMD

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
    csrrd   $sp, 0x502
.endm

.global _run_guest
_run_guest:
    move    $t1, $ra
    addi.d  $t0, $a0, 792
    csrwr   $t0, 0x33
    move    $sp, $a0
    PREPARE_GUEST_ENTRY
    RESTORE_GUEST_REGS
    ertn
    jr      $t1

.global _guest_exit
_guest_exit:
    b       vmexit_trampoline

.balign 4096
.global _guest_tlb_refill_vector
_guest_tlb_refill_vector:
    csrwr   $t0, CSR_TLBRSAVE
    csrrd   $t0, CSR_PGD

    lddir   $t0, $t0, 3
    beqz    $t0, .Lguest_stage2_tlb_invalid

    lddir   $t0, $t0, 2
    beqz    $t0, .Lguest_stage2_tlb_invalid

    lddir   $t0, $t0, 1
    beqz    $t0, .Lguest_stage2_tlb_invalid

    ldpte   $t0, 0
    ldpte   $t0, 1
    tlbfill
    csrrd   $t0, CSR_TLBRSAVE
    ertn

.Lguest_stage2_tlb_invalid:
    csrrd   $t0, CSR_TLBRSAVE
    csrwr   $t0, CSR_SAVE0
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    li.d    $a0, 0
    b       _guest_exit

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

    /*
     * Hardware interrupts use EENTRY + (64 + vector) * vec_size.  With VS=5
     * each slot is 0x80 bytes, so local interrupt 11 (timer) lands at 0x2580.
     */
    .org 0x2000
    b       _irq_handler_vector
    .org 0x2080
    b       _irq_handler_vector
    .org 0x2100
    b       _irq_handler_vector
    .org 0x2180
    b       _irq_handler_vector
    .org 0x2200
    b       _irq_handler_vector
    .org 0x2280
    b       _irq_handler_vector
    .org 0x2300
    b       _irq_handler_vector
    .org 0x2380
    b       _irq_handler_vector
    .org 0x2400
    b       _irq_handler_vector
    .org 0x2480
    b       _irq_handler_vector
    .org 0x2500
    b       _irq_handler_vector
    .org 0x2580
    b       _irq_handler_vector
    .org 0x2600
    b       _irq_handler_vector
    .org 0x2680
    b       _irq_handler_vector
    .org 0x2700
    b       _irq_handler_vector
    .org 0x2780
    b       _irq_handler_vector
    .org 0x2800
    b       _irq_handler_vector
    .org 0x2880
    b       _irq_handler_vector
    .org 0x2900
    b       _irq_handler_vector
    .org 0x2980
    b       _irq_handler_vector
    .org 0x2a00
    b       _irq_handler_vector
    .org 0x2a80
    b       _irq_handler_vector
    .org 0x2b00
    b       _irq_handler_vector
    .org 0x2b80
    b       _irq_handler_vector
    .org 0x2c00
    b       _irq_handler_vector
    .org 0x2c80
    b       _irq_handler_vector
    .org 0x2d00
    b       _irq_handler_vector
    .org 0x2d80
    b       _irq_handler_vector
    .org 0x2e00
    b       _irq_handler_vector
    .org 0x2e80
    b       _irq_handler_vector
    .org 0x2f00
    b       _irq_handler_vector
    .org 0x2f80
    b       _irq_handler_vector

_irq_handler_vector:
    csrwr   $t0, CSR_SAVE0
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    li.d    $a0, 1
    b       _guest_exit

_sync_handler_vector:
    csrwr   $t0, CSR_SAVE0
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    li.d    $a0, 0
    b       _guest_exit

_hvc_handler_vector:
    csrwr   $t0, CSR_SAVE0
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, 0x33
    addi.d  $sp, $sp, -792
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    li.d    $a0, 0
    b       _guest_exit
