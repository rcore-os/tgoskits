// Typical exception vector table code.
.balign 0x800
.global __vector_table
__vector_table:
    curr_el_sp0_sync: // The exception handler for the synchronous
    B .
    // exception from the current EL using SP0.
    .balign 0x80
    curr_el_sp0_irq: // The exception handler for the IRQ exception
    B .
    // from the current EL using SP0.
    .balign 0x80
    curr_el_sp0_fiq: // The exception handler for the FIQ exception
    B .
    // from the current EL using SP0.
    .balign 0x80
    curr_el_sp0_serror: // The exception handler for the system error 
    B .
    .balign 0x80
    curr_el_spx_sync:
    B {sync_handler}
    .balign 0x80
    curr_el_spx_irq: 
	B {irq_handler}
    .balign 0x80
    curr_el_spx_frq: 
    B {fiq_handler}
    .balign 0x80
    curr_el_spx_serror: // The exception handler for the system error 
    B {serror_handler}
    // exception from the current EL using the
    // current SP.
    .balign 0x80
    lower_el_aarch64_sync: // The exception handler for the synchronous
    B .
    // exception from a lower EL (AArch64).
 
    .balign 0x80
    lower_el_aarch64_irq: // The exception handler for the IRQ exception 
    // from a lower EL (AArch64).
    .balign 0x80
    lower_el_aarch64_fiq: // The exception handler for the FIQ exception 
    // from a lower EL (AArch64).
    .balign 0x80
    lower_el_aarch64_serror: // The exception handler for the system error 
    // exception from a lower EL(AArch64).
    .balign 0x80
    lower_el_aarch32_sync: // The exception handler for the synchronous
    // exception from a lower EL(AArch32).
    .balign 0x80
    lower_el_aarch32_irq: // The exception handler for the IRQ exception 
    // from a lower EL (AArch32).
    .balign 0x80
    lower_el_aarch32_fiq: // The exception handler for the FIQ exception 
    // from a lower EL (AArch32).
    .balign 0x80
    lower_el_aarch32_serror: // The exception handler for the system error
    // exception from a lower EL(AArch32).

// ------------------------------------------------------------