#[cfg(feature = "stage2-tlb")]
#[path = "stage2_tlb.rs"]
mod stage2_tlb;
use ax_cpu::{
    interrupt, registers,
    virtualization::{
        ExitKind, GuestHostTrap, HostIrqConfig, PerCpu, Vcpu, enter_guest, guest_vector,
    },
};

struct HostTrap;
#[trait_ffi::impl_extern_trait]
impl GuestHostTrap for HostTrap {
    fn current_irq(_: ax_cpu::trap::InterruptedContext) {
        panic!("host IRQs stay masked during the machine test");
    }
}

core::arch::global_asm!(
    ".arch armv8",
    ".section .text",
    ".balign 4",
    ".global cpu_test_arm_guest",
    "cpu_test_arm_guest:",
    "fmov x1, d0",
    "mov x0, #0x123",
    "fmov d0, x0",
    "mrs x2, tpidr_el0",
    "hvc #0",
    "fmov x1, d0",
    "mov x0, #0x456",
    "fmov d0, x0",
    "mrs x2, tpidr_el0",
    "hvc #0",
);
unsafe extern "C" {
    fn cpu_test_arm_guest();
}

pub fn run() {
    assert_eq!(registers::current_exception_level(), 2);
    let irq = interrupt::irqs_enabled();
    interrupt::disable_irqs();
    let anchor = registers::read_tpidr_el2();
    let current = registers::read_sp_el0();
    let tls: u64;
    // SAFETY: one pinned EL2 CPU owns this complete IRQ-masked machine test.
    unsafe {
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) tls, options(nostack));
    }
    let mut cpu = PerCpu::new();
    unsafe {
        cpu.enable(guest_vector()).unwrap();
    }
    let mut guest = Vcpu::default();
    guest.context.elr = ax_hal::mem::virt_to_phys(ax_cpu::VirtAddr::from_usize(
        cpu_test_arm_guest as *const () as usize,
    ))
    .as_usize() as u64;
    // This trusted straight-line guest has no data accesses or address translation.
    guest.system.hcr_el2 = (1 << 31) | (1 << 19);
    guest.system.sctlr_el1 = 0x30c5_0830;
    guest.system.cpacr_el1 = 3 << 20;
    guest.system.tpidr_el0 = 0xabc;
    guest.set_host_irq_interface(HostIrqConfig::gicv3());
    guest.timer.offset = 0x111;
    guest.timer.compare = 0x999;
    let original_offset: u64;
    let original_compare: u64;
    let original_control: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntvoff_el2", out(reg) original_offset, options(nostack));
        core::arch::asm!("mrs {}, cntv_cval_el0", out(reg) original_compare, options(nostack));
        core::arch::asm!("mrs {}, cntv_ctl_el0", out(reg) original_control, options(nostack));
        core::arch::asm!("msr cntv_ctl_el0, {}", "isb", in(reg) 2u64, options(nostack));
        core::arch::asm!("msr cntvoff_el2, {}", in(reg) 0x222u64, options(nostack));
        core::arch::asm!("msr cntv_cval_el0, {}", "isb", in(reg) u64::MAX, options(nostack));
    }
    for (expected, previous) in [(0x123, 0), (0x456, 0x123)] {
        // SAFETY: valid trusted EL1 code, CPU-owned vector and IRQ exclusion;
        // roots stay bare and the guest cannot reference host data memory.
        let exit = unsafe { enter_guest(&mut guest) };
        let restored_tls: u64;
        let offset: u64;
        let compare: u64;
        let control: u64;
        unsafe {
            core::arch::asm!("msr esr_el2, xzr", options(nostack));
            core::arch::asm!("mrs {}, tpidr_el0", out(reg) restored_tls, options(nostack));
            core::arch::asm!("mrs {}, cntvoff_el2", out(reg) offset, options(nostack));
            core::arch::asm!("mrs {}, cntv_cval_el0", out(reg) compare, options(nostack));
            core::arch::asm!("mrs {}, cntv_ctl_el0", out(reg) control, options(nostack));
        }
        assert_eq!(exit.kind, ExitKind::Synchronous);
        assert_eq!((exit.syndrome >> 26) & 63, 0x16, "guest must exit by HVC64");
        assert_eq!(exit.pc, guest.context.elr);
        assert_eq!(guest.context.gpr[0], expected);
        assert_eq!(guest.context.gpr[1], previous);
        assert_eq!(guest.context.gpr[2], 0xabc);
        assert_eq!(guest.fp.regs[0] as u64, expected);
        assert_eq!(restored_tls, tls);
        assert_eq!(registers::read_tpidr_el2(), anchor);
        assert_eq!(registers::read_sp_el0(), current);
        assert_eq!(offset, 0x222);
        assert_eq!(compare, u64::MAX);
        assert_eq!(control & 3, 2);
        assert_eq!(guest.timer.offset, 0x111);
        assert_eq!(guest.timer.compare, 0x999);
        assert!(!interrupt::irqs_enabled());
    }
    unsafe {
        core::arch::asm!("msr cntv_ctl_el0, xzr", "isb", options(nostack));
        core::arch::asm!("msr cntvoff_el2, {}", in(reg) original_offset, options(nostack));
        core::arch::asm!("msr cntv_cval_el0, {}", in(reg) original_compare, options(nostack));
        core::arch::asm!("msr cntv_ctl_el0, {}", "isb", in(reg) original_control, options(nostack));
        cpu.disable().unwrap();
    }
    if irq {
        interrupt::enable_irqs();
    }
    #[cfg(feature = "stage2-tlb")]
    stage2_tlb::run();
    std::println!("CPU_GUEST_ENTRY_OK");
}
