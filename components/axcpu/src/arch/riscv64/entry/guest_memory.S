// Copyright (c) 2022 by Rivos Inc.
// Licensed under the Apache License, Version 2.0, see LICENSE for details.
// SPDX-License-Identifier: Apache-2.0

// Very unoptimized memcpy() to/from guest memory functions, using the HLV/HSV instructions.

// Adds the faulting instruction and fixup target to the exception table.
.macro add_extable from, to
.pushsection __nofault_ex_table, "a"
.balign      4
.word        \from - _nofault_ex_table_start
.word        \to - _nofault_ex_table_start
.popsection
.endm
.option push
.option arch, +h
.section .text

// Copy to guest virtual memory using HSV.
.global __ax_cpu_copy_to_guest
.hidden __ax_cpu_copy_to_guest
__ax_cpu_copy_to_guest:
    // handle_trap assumes t0 holds the address of where we want to jump to when we encounter
    // a fault and will stick SCAUSE in t1.
    la    t0, .Lguest_copy_return
    // .Lguest_copy_return assumes the return value is in t2.
    mv    t2, zero
1:
    beq   t2, a2, .Lguest_copy_return
    lb    t3, (a1)
2:
    hsv.b t3, (a0)
    add_extable 2b, .Lguest_copy_return
    addi  a0, a0, 1
    addi  a1, a1, 1
    addi  t2, t2, 1
    j     1b

// Copy from guest virtual memory using HLV.
.global __ax_cpu_copy_from_guest
.hidden __ax_cpu_copy_from_guest
__ax_cpu_copy_from_guest:
    // handle_trap assumes t0 holds the address of where we want to jump to when we encounter
    // a fault and will stick SCAUSE in t1.
    la    t0, .Lguest_copy_return
    // .Lguest_copy_return assumes the return value is in t2.
    mv    t2, zero
1:
    beq   t2, a2, .Lguest_copy_return
2:
    hlv.b t3, (a1)
    add_extable 2b, .Lguest_copy_return
    sb    t3, (a0)
    addi  a0, a0, 1
    addi  a1, a1, 1
    addi  t2, t2, 1
    j     1b

// Fetch an instruction from guest memory using HLVX. Only supports 2 or 4 byte instructions.
//
// Arguments:
//   A0: Guest address of the instruction to fetch, using the translation modes/tables currently
//       programmed in HGATP and VSATP.
//   A1: Pointer to a u32 where the instruction will be written.
//   A2: Pointer to fault info written if the access faults.
//
// Returns 1 on error and writes scause, stval, and htval to A2. HLVX checks execute
// permission but reports load-class exceptions, so the Rust side needs the raw
// trap CSRs to recover the guest-visible fetch semantics.
.global __ax_cpu_fetch_guest_instruction
.hidden __ax_cpu_fetch_guest_instruction
__ax_cpu_fetch_guest_instruction:
    // The exception table fixup redirects a faulting HLVX here.
    la    t0, 4f
1:
    hlvx.hu t2, (a0)
    add_extable 1b, 4f
    sh    t2, (a1)
    addi  a0, a0, 2
    addi  a1, a1, 2
    // If it's a compressed instrution (bits [1:0] != 'b11) then we're done.
    li    t3, 3
    and   t2, t2, t3
    bne   t2, t3, 3f
    // Load the next half-word.
2:
    hlvx.hu t2, (a0)
    add_extable 2b, 4f
    sh    t2, (a1)
3:
    mv    a0, zero
    ret
4:
    // Took a fault, record trap CSRs and return 1.
    csrr  t2, scause
    sd    t2, {fault_scause}(a2)
    csrr  t2, stval
    sd    t2, {fault_stval}(a2)
    csrr  t2, htval
    sd    t2, {fault_htval}(a2)
    li    a0, 1
    ret

.align 2
.Lguest_copy_return:
    mv    a0, t2
    ret
.option pop
