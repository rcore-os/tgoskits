pub fn run() {
    assert_eq!(ax_cpu::registers::current_exception_level(), 2);
    let irqs = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let original: u64;
    let vector: u64;
    // SAFETY: this one-CPU EL2 test runs without guests and with IRQs masked.
    unsafe {
        core::arch::asm!("mrs {}, hcr_el2", out(reg) original, options(nostack));
        core::arch::asm!("mrs {}, vbar_el2", out(reg) vector, options(nostack));
    }
    let expected = original | (1 << 13);
    unsafe {
        core::arch::asm!("msr hcr_el2, {}", "isb", in(reg) expected, options(nostack));
    }
    use ax_cpu::virtualization::{PerCpu, VirtualizationError};
    let mut cpu = PerCpu::new();
    // SAFETY: the original EL2 vector remains executable throughout this
    // IRQ-masked test; no guest executes or owns the current CPU.
    unsafe {
        assert_eq!(
            cpu.enable(ax_cpu::VirtAddr::from_usize(vector as usize + 4)),
            Err(VirtualizationError::InvalidVector)
        );
        assert!(!cpu.is_enabled());
        cpu.enable(ax_cpu::VirtAddr::from_usize(vector as usize))
            .unwrap();
        assert!(cpu.is_enabled());
        assert_eq!(
            cpu.enable(ax_cpu::VirtAddr::from_usize(vector as usize)),
            Err(VirtualizationError::AlreadyEnabled)
        );
        cpu.disable().unwrap();
        assert!(!cpu.is_enabled());
        assert_eq!(cpu.disable(), Err(VirtualizationError::NotEnabled));
    }
    let actual: u64;
    let restored_vector: u64;
    unsafe {
        core::arch::asm!("mrs {}, hcr_el2", out(reg) actual, options(nostack));
        core::arch::asm!("mrs {}, vbar_el2", out(reg) restored_vector, options(nostack));
        core::arch::asm!("msr hcr_el2, {}", "isb", in(reg) original, options(nostack));
    }
    assert_eq!(restored_vector, vector);
    assert_eq!(
        actual, expected,
        "closing virtualization must restore host HCR_EL2"
    );
    if irqs {
        ax_cpu::interrupt::enable_irqs();
    }
    std::println!("CPU_VIRTUALIZATION_LIFECYCLE_OK");
}
