pub fn run() {
    assert!(
        ax_cpu::capability::has_hypervisor_extension(),
        "LVZ test requires LVZ hardware"
    );
    let irqs = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let original: usize;
    // SAFETY: this one-CPU test owns the idle LVZ bank with IRQs disabled.
    unsafe {
        core::arch::asm!("gcsrrd {}, 0xc", out(reg) original, options(nostack));
    }
    let expected = 0x1000usize;
    unsafe {
        core::arch::asm!("gcsrwr {}, 0xc", inout(reg) expected => _, options(nostack));
    }
    let mut cpu = ax_cpu::virtualization::PerCpu::new();
    unsafe {
        cpu.enable().unwrap();
    }
    unsafe {
        core::arch::asm!("gcsrwr {}, 0xc", inout(reg) 0x2000usize => _, options(nostack));
    }
    unsafe {
        cpu.disable().unwrap();
    }
    let restored: usize;
    unsafe {
        core::arch::asm!("gcsrrd {}, 0xc", out(reg) restored, options(nostack));
        core::arch::asm!("gcsrwr {}, 0xc", inout(reg) original => _, options(nostack));
    }
    assert_eq!(
        restored, expected,
        "LVZ disable must restore the guest vector bank"
    );
    if irqs {
        ax_cpu::interrupt::enable_irqs();
    }
    std::println!("CPU_VIRTUALIZATION_LIFECYCLE_OK");
}
