#![no_std]
#![no_main]

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        extern crate ax_plat_x86_pc as axplat_crate;
    } else if #[cfg(target_arch = "aarch64")] {
        extern crate ax_plat_aarch64_qemu_virt as axplat_crate;
    } else if #[cfg(target_arch = "riscv64")] {
        extern crate ax_plat_riscv64_qemu_virt as axplat_crate;
    } else if #[cfg(target_arch = "loongarch64")] {
        extern crate ax_plat_loongarch64_qemu_virt as axplat_crate;
    } else {
        compile_error!("Unsupported target architecture");
    }
}

mod irq;
use irq::*;

fn init_kernel(cpu_id: usize, arg: usize) {
    ax_plat::percpu::init_primary(cpu_id);

    // Initialize trap, console, time.
    ax_plat::init::init_early(cpu_id, arg);

    // Initialize platform peripherals, such as IRQ handlers.
    ax_plat::init::init_later(cpu_id, arg);
}

#[ax_plat::main]
fn main(cpu_id: usize, arg: usize) -> ! {
    init_kernel(cpu_id, arg);

    ax_plat::console_println!("Hello, ArceOS!");
    ax_plat::console_println!("cpu_id = {cpu_id}, arg = {arg:#x}");

    init_irq();
    test_irq();

    ax_plat::power::system_off();
}

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    ax_plat::console_println!("{info}");
    ax_plat::power::system_off()
}
