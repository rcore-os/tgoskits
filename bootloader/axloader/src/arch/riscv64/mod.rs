//! Native RISC-V EFI entry before the firmware-backed Rust allocator is used.

core::arch::global_asm!(
    r#"
/* Firmware enters with a0 = image handle, a1 = system table and a live stack.
 * The PE loader owns the whole image and zeroes the uninitialized data tail.
 * No Rust code, GOT load or global reference may precede self-relocation.
 */
.option push
.option norelax
.option norvc
.section .text.efi_entry, "ax", @progbits
.balign 4
.global axloader_efi_entry
.type axloader_efi_entry, @function
axloader_efi_entry:
    addi sp, sp, -32
    sd ra, 24(sp)
    sd gp, 16(sp)
    sd a0, 8(sp)
    sd a1, 0(sp)
    lla t0, __image_start
    lla t1, __rela_start
    lla t2, __rela_end
    lla t6, __image_end
1:
    beq t1, t2, 4f
    addi t3, t1, 24
    bltu t2, t3, 5f
    ld t4, 8(t1)
    beqz t4, 3f
    li t3, 3 /* R_RISCV_RELATIVE, with no symbol index. */
    bne t4, t3, 5f
    ld t3, 0(t1)
    andi t4, t3, 7
    bnez t4, 5f
    sub t4, t6, t0
    addi t4, t4, -8
    bltu t4, t3, 5f
    add t3, t0, t3
    lla t4, __data_start
    bltu t3, t4, 5f
    ld t5, 16(t1)
    sub t4, t6, t0
    bltu t4, t5, 5f
    add t5, t0, t5
    sd t5, 0(t3)
3:
    addi t1, t1, 24
    j 1b
4:
    fence.i
    lla gp, __global_pointer$
    ld a0, 8(sp)
    ld a1, 0(sp)
    lla t0, efi_main
    jalr ra, t0, 0
    j 6f
5:
    /* EFI_UNSUPPORTED: returning never exposes partially relocated Rust state. */
    li a0, 1
    slli a0, a0, 63
    ori a0, a0, 3
6:
    ld gp, 16(sp)
    ld ra, 24(sp)
    addi sp, sp, 32
    ret
.size axloader_efi_entry, . - axloader_efi_entry
.option pop
"#
);
