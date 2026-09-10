// Four-level, 4 KiB LoongArch refill, shared by boot and runtime.
// This handler uses only t0 and TLBRSAVE, without a stack or CPU-local area.
.section .text.ax_cpu.tlb_refill, "ax"
.balign 4096
.global __ax_cpu_tlb_refill
.hidden __ax_cpu_tlb_refill
.type __ax_cpu_tlb_refill, @function
__ax_cpu_tlb_refill:
    csrwr   $t0, 0x8b
    csrrd   $t0, 0x1b

    AX_CPU_WALK_4K .Ltlb_invalid

    b       .Ltlb_refill

.Ltlb_invalid:
    // Match the invalid-table entries used by Linux. In particular, do not
    // reuse TLBREHI here: it contains the faulting virtual page number, not an
    // EntryLo physical page number. A zero EntryLo keeps V clear and lets the
    // CPU report the original load, store, or fetch fault type.
    csrwr   $r0, 0x8c
    csrwr   $r0, 0x8d

.Ltlb_refill:
    tlbfill
    csrrd   $t0, 0x8b
    ertn

.size __ax_cpu_tlb_refill, . - __ax_cpu_tlb_refill
