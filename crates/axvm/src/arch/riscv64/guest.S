
/// Enter the guest given in `VmCpuRegisters` from `a0`
.global _run_guest
_run_guest:
    /* Save hypervisor state */

    /* Save hypervisor GPRs (except T0-T6 and a0, which is GuestInfo and stashed in sscratch) */
    sd   ra, ({hyp_ra})(a0)
    sd   gp, ({hyp_gp})(a0)
    sd   tp, ({hyp_tp})(a0)
    sd   s0, ({hyp_s0})(a0)
    sd   s1, ({hyp_s1})(a0)
    sd   a1, ({hyp_a1})(a0)
    sd   a2, ({hyp_a2})(a0)
    sd   a3, ({hyp_a3})(a0)
    sd   a4, ({hyp_a4})(a0)
    sd   a5, ({hyp_a5})(a0)
    sd   a6, ({hyp_a6})(a0)
    sd   a7, ({hyp_a7})(a0)
    sd   s2, ({hyp_s2})(a0)
    sd   s3, ({hyp_s3})(a0)
    sd   s4, ({hyp_s4})(a0)
    sd   s5, ({hyp_s5})(a0)
    sd   s6, ({hyp_s6})(a0)
    sd   s7, ({hyp_s7})(a0)
    sd   s8, ({hyp_s8})(a0)
    sd   s9, ({hyp_s9})(a0)
    sd   s10, ({hyp_s10})(a0)
    sd   s11, ({hyp_s11})(a0)
    sd   sp, ({hyp_sp})(a0)

    /* Swap in guest CSRs. */
    ld    t1, ({guest_sstatus})(a0)
    csrrw t1, sstatus, t1
    sd    t1, ({hyp_sstatus})(a0)

    ld    t1, ({guest_hstatus})(a0)
    csrrw t1, hstatus, t1
    sd    t1, ({hyp_hstatus})(a0)

    ld    t1, ({guest_scounteren})(a0)
    csrrw t1, scounteren, t1
    sd    t1, ({hyp_scounteren})(a0)

    ld    t1, ({guest_sepc})(a0)
    csrw  sepc, t1

    /* Set stvec so that hypervisor resumes after the sret when the guest exits. */
    la    t1, _guest_exit
    csrrw t1, stvec, t1
    sd    t1, ({hyp_stvec})(a0)

    /* Save sscratch and replace with pointer to GuestInfo. */
    csrrw t1, sscratch, a0
    sd    t1, ({hyp_sscratch})(a0)

    /* Restore the gprs from this GuestInfo */
    ld   ra, ({guest_ra})(a0)
    ld   gp, ({guest_gp})(a0)
    ld   tp, ({guest_tp})(a0)
    ld   s0, ({guest_s0})(a0)
    ld   s1, ({guest_s1})(a0)
    ld   a1, ({guest_a1})(a0)
    ld   a2, ({guest_a2})(a0)
    ld   a3, ({guest_a3})(a0)
    ld   a4, ({guest_a4})(a0)
    ld   a5, ({guest_a5})(a0)
    ld   a6, ({guest_a6})(a0)
    ld   a7, ({guest_a7})(a0)
    ld   s2, ({guest_s2})(a0)
    ld   s3, ({guest_s3})(a0)
    ld   s4, ({guest_s4})(a0)
    ld   s5, ({guest_s5})(a0)
    ld   s6, ({guest_s6})(a0)
    ld   s7, ({guest_s7})(a0)
    ld   s8, ({guest_s8})(a0)
    ld   s9, ({guest_s9})(a0)
    ld   s10, ({guest_s10})(a0)
    ld   s11, ({guest_s11})(a0)
    ld   t0, ({guest_t0})(a0)
    ld   t1, ({guest_t1})(a0)
    ld   t2, ({guest_t2})(a0)
    ld   t3, ({guest_t3})(a0)
    ld   t4, ({guest_t4})(a0)
    ld   t5, ({guest_t5})(a0)
    ld   t6, ({guest_t6})(a0)
    ld   sp, ({guest_sp})(a0)
    ld   a0, ({guest_a0})(a0)

    sret

.align 2
_guest_exit:
    /* Pull GuestInfo out of sscratch, swapping with guest's a0 */
    csrrw a0, sscratch, a0

    /* Save guest GPRs. */
    sd   ra, ({guest_ra})(a0)
    sd   gp, ({guest_gp})(a0)
    sd   tp, ({guest_tp})(a0)
    sd   s0, ({guest_s0})(a0)
    sd   s1, ({guest_s1})(a0)
    sd   a1, ({guest_a1})(a0)
    sd   a2, ({guest_a2})(a0)
    sd   a3, ({guest_a3})(a0)
    sd   a4, ({guest_a4})(a0)
    sd   a5, ({guest_a5})(a0)
    sd   a6, ({guest_a6})(a0)
    sd   a7, ({guest_a7})(a0)
    sd   s2, ({guest_s2})(a0)
    sd   s3, ({guest_s3})(a0)
    sd   s4, ({guest_s4})(a0)
    sd   s5, ({guest_s5})(a0)
    sd   s6, ({guest_s6})(a0)
    sd   s7, ({guest_s7})(a0)
    sd   s8, ({guest_s8})(a0)
    sd   s9, ({guest_s9})(a0)
    sd   s10, ({guest_s10})(a0)
    sd   s11, ({guest_s11})(a0)
    sd   t0, ({guest_t0})(a0)
    sd   t1, ({guest_t1})(a0)
    sd   t2, ({guest_t2})(a0)
    sd   t3, ({guest_t3})(a0)
    sd   t4, ({guest_t4})(a0)
    sd   t5, ({guest_t5})(a0)
    sd   t6, ({guest_t6})(a0)
    sd   sp, ({guest_sp})(a0)

    /* Save Guest a0 after recovering from sscratch. */
    csrr  t0, sscratch
    sd    t0, ({guest_a0})(a0)

_restore_csrs:
    /* Swap in hypervisor CSRs. */
    ld    t1, ({hyp_sstatus})(a0)
    csrrw t1, sstatus, t1
    sd    t1, ({guest_sstatus})(a0)

    ld    t1, ({hyp_hstatus})(a0)
    csrrw t1, hstatus, t1
    sd    t1, ({guest_hstatus})(a0)

    ld    t1, ({hyp_scounteren})(a0)
    csrrw t1, scounteren, t1
    sd    t1, ({guest_scounteren})(a0)

    ld    t1, ({hyp_stvec})(a0)
    csrw  stvec, t1

    ld    t1, ({hyp_sscratch})(a0)
    csrw  sscratch, t1

    /* Save guest EPC. */
    csrr  t1, sepc
    sd    t1, ({guest_sepc})(a0)


    /* Restore hypervisor GPRs. */
    ld   ra, ({hyp_ra})(a0)
    ld   gp, ({hyp_gp})(a0)
    ld   tp, ({hyp_tp})(a0)
    ld   s0, ({hyp_s0})(a0)
    ld   s1, ({hyp_s1})(a0)
    ld   a1, ({hyp_a1})(a0)
    ld   a2, ({hyp_a2})(a0)
    ld   a3, ({hyp_a3})(a0)
    ld   a4, ({hyp_a4})(a0)
    ld   a5, ({hyp_a5})(a0)
    ld   a6, ({hyp_a6})(a0)
    ld   a7, ({hyp_a7})(a0)
    ld   s2, ({hyp_s2})(a0)
    ld   s3, ({hyp_s3})(a0)
    ld   s4, ({hyp_s4})(a0)
    ld   s5, ({hyp_s5})(a0)
    ld   s6, ({hyp_s6})(a0)
    ld   s7, ({hyp_s7})(a0)
    ld   s8, ({hyp_s8})(a0)
    ld   s9, ({hyp_s9})(a0)
    ld   s10, ({hyp_s10})(a0)
    ld   s11, ({hyp_s11})(a0)
    ld   sp, ({hyp_sp})(a0)

    ret