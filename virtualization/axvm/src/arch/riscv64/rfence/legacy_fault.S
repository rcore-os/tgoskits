// Position-independent VS guest: exercise all legacy RFENCE mask loads.
.pushsection .rodata.axvm_legacy_fault, "a"
.option push
.option norelax
.option norvc
.balign 4096
.global axvm_legacy_fault_start
axvm_legacy_fault_start:
    lla t0, root
    lla t1, middle
    srli t1, t1, 2
    ori t1, t1, 1
    sd t1, 8(t0)
    lla t1, middle
    li t2, 0x100000cf // Identity-map the first 2 MiB at 0x40000000.
    sd t2, 0(t1)
    srli t0, t0, 12
    li t1, 8 << 60
    or t0, t0, t1
    csrw satp, t0
    sfence.vma
    lla t0, fault
    csrw stvec, t0
    li s3, 5
test_call:
    li s1, 0x80000000 // Unmapped VS address, with valid guest RAM elsewhere.
    li s4, 2
test_pointer:
    li s2, 0
    lla s0, mask_ecall
    mv a0, s1
    li a1, 0x1234
    li a2, 4096
    li a3, 0
    li a6, 0
    mv a7, s3
mask_ecall:
    ecall
    li t0, 1
    bne s2, t0, failed
    li t0, 0x1234
    bne a1, t0, failed
    addi s4, s4, -1
    beqz s4, next_call
    // Four readable bytes followed by a VS page fault during the same mask.
    li s1, 0x401ffffc
    j test_pointer
next_call:
    addi s3, s3, 1
    li t0, 8
    bne s3, t0, test_call
    li t1, 1
    j finish
fault:
    csrr t0, scause
    li t1, 13
    bne t0, t1, failed
    csrr t0, sepc
    bne t0, s0, failed
    csrr t0, stval
    mv t1, s1
    li t2, 0x401ffffc
    bne s1, t2, 1f
    addi t1, t1, 4
1:  bne t0, t1, failed
    addi s2, s2, 1
    addi s0, s0, 4
    csrw sepc, s0
    sret
failed:
    li t1, 2
finish:
    li t0, 0x40010000
    sd t1, 0(t0)
    fence rw, rw
2:  j 2b
.balign 4096
root:
    .zero 4096
middle:
    .zero 4096
.global axvm_legacy_fault_end
axvm_legacy_fault_end:
.option pop
.popsection
