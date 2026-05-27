// Copyright (c) 2022 by Rivos Inc.
// Licensed under the Apache License, Version 2.0, see LICENSE for details.
// SPDX-License-Identifier: Apache-2.0

// Very unoptimized memcpy() to/from guest memory functions, using the HLV/HSV instructions.

// Adds the faulting instruction and fixup target to the exception table.
.macro add_extable from, to
.pushsection __ex_table, "a"
.balign      8
.quad        \from
.quad        \to
.popsection
.endm
.option push
.option arch, +h
.section .text

// memcpy() to a guest physical address using HSV.
.global _copy_to_guest
_copy_to_guest:
    // handle_trap assumes t0 holds the address of where we want to jump to when we encounter
    // a fault and will stick SCAUSE in t1.
    la    t0, _ret_from_copy
    // _ret_from_copy assumes the return value is in t2.
    mv    t2, zero
1:
    beq   t2, a2, _ret_from_copy
    lb    t3, (a1)
2:
    hsv.b t3, (a0)
    add_extable 2b, _ret_from_copy
    addi  a0, a0, 1
    addi  a1, a1, 1
    addi  t2, t2, 1
    j     1b

// memcpy() from a guest physical address using HLV.
.global _copy_from_guest
_copy_from_guest:
    // handle_trap assumes t0 holds the address of where we want to jump to when we encounter
    // a fault and will stick SCAUSE in t1.
    la    t0, _ret_from_copy
    // _ret_from_copy assumes the return value is in t2.
    mv    t2, zero
1:
    beq   t2, a2, _ret_from_copy
2:
    hlv.b t3, (a1)
    add_extable 2b, _ret_from_copy
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
.global _fetch_guest_instruction
_fetch_guest_instruction:
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
    sd    t2, 0(a2)
    csrr  t2, stval
    sd    t2, 8(a2)
    csrr  t2, htval
    sd    t2, 16(a2)
    li    a0, 1
    ret

// memcpy() to a user address.
.global _copy_to_user
_copy_to_user:
   // handle_trap assumes t0 holds the address of where we want to jump to when we encounter
   // a fault and will stick SCAUSE in t1.
   la    t0, _ret_from_copy
   // _ret_from_copy assumes the return value is in t2.
   mv    t2, zero
1:
   beq   t2, a2, _ret_from_copy
   lb    t3, (a1)
2:
   sb t3, (a0)
   add_extable 2b, _ret_from_copy
   addi  a0, a0, 1
   addi  a1, a1, 1
   addi  t2, t2, 1
   j     1b

// memcpy() from a user address.
.global _copy_from_user
_copy_from_user:
   // handle_trap assumes t0 holds the address of where we want to jump to when we encounter
   // a fault and will stick SCAUSE in t1.
   la    t0, _ret_from_copy
   // _ret_from_copy assumes the return value is in t2.
   mv    t2, zero
1:
   beq   t2, a2, _ret_from_copy
2:
   lb t3, (a1)
   add_extable 2b, _ret_from_copy
   sb    t3, (a0)
   addi  a0, a0, 1
   addi  a1, a1, 1
   addi  t2, t2, 1
   j     1b

.align 2
_ret_from_copy:
    mv    a0, t2
    ret
.option pop
