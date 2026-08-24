/* SPDX-License-Identifier: Apache-2.0 */

#ifndef AXVISOR_RT_NOISE_SECONDS
#define AXVISOR_RT_NOISE_SECONDS 180
#endif

#ifndef AXVISOR_RT_NOISE_COUNTER_HZ
#define AXVISOR_RT_NOISE_COUNTER_HZ 24000000
#endif

#if AXVISOR_RT_NOISE_SECONDS < 1
#error "AXVISOR_RT_NOISE_SECONDS must be positive"
#endif

#if AXVISOR_RT_NOISE_COUNTER_HZ < 1000000
#error "AXVISOR_RT_NOISE_COUNTER_HZ must be at least 1 MHz"
#endif

    .section .text.boot, "ax"
    .balign 4096
    .global _start
    .type _start, %function

_start:
    /*
     * Run a bounded CPU workload in the guest virtual-counter domain.  This
     * minimal guest cannot assume that CNTFRQ_EL0 has been initialized, so
     * the board build supplies the RK3588 architected-counter frequency.
     */
    ldr     x0, =AXVISOR_RT_NOISE_COUNTER_HZ
    mov     x1, #AXVISOR_RT_NOISE_SECONDS
    mul     x0, x0, x1
    mrs     x1, cntvct_el0
    add     x0, x0, x1

    mov     x2, #1
    mov     x3, #0
1:
    add     x2, x2, #1
    eor     x3, x3, x2, ror #17
    mrs     x1, cntvct_el0
    cmp     x1, x0
    b.lo    1b

    /* PSCI SYSTEM_OFF: 0x8400_0008. */
    movz    x0, #0x0008
    movk    x0, #0x8400, lsl #16
    smc     #0

2:
    wfe
    b       2b

    .size _start, . - _start
