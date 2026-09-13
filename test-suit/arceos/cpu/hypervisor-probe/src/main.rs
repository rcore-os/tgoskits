#![no_std]
#![no_main]
extern crate ax_std as std;

use ax_cpu::{capability::has_hypervisor_extension, interrupt, registers};

fn trap_vector() -> usize {
    let value;
    // SAFETY: this independent observation only reads the supervisor vector.
    unsafe { core::arch::asm!("csrr {}, stvec", out(reg) value, options(nostack)) };
    value
}

#[unsafe(no_mangle)]
fn main() {
    let original_irq = interrupt::irqs_enabled();
    // Both images have one hart, so enabled-IRQ checks cannot migrate to
    // another CPU with a different vector or scratch binding.
    for enabled in [false, true] {
        if enabled {
            interrupt::enable_irqs();
        } else {
            interrupt::disable_irqs();
        }
        let vector = trap_vector();
        let scratch = registers::read_sscratch();
        let tls = registers::read_tp();
        for _ in 0..2 {
            let supported = has_hypervisor_extension();
            assert_eq!(supported, cfg!(feature = "expect-h"));
            assert_eq!(interrupt::irqs_enabled(), enabled);
            assert_eq!(trap_vector(), vector);
            assert_eq!(registers::read_sscratch(), scratch);
            assert_eq!(registers::read_tp(), tls);
        }
    }
    if original_irq {
        interrupt::enable_irqs();
    } else {
        interrupt::disable_irqs();
    }
    std::println!("CPU_HYPERVISOR_PROBE_OK");
    std::process::exit(0);
}
