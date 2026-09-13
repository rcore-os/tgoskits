// x86_64 has no someboot-level IRQ abstraction: every interrupt controller
// (local APIC, I/O APIC) is driven by somehal through `x86-apic-driver`, and
// the boot path only owns traps.
