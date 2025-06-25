#![no_std]
#![no_main]

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        extern crate axplat_x86_pc;
        use axplat_x86_pc::config as axplat_config;
    } else if #[cfg(target_arch = "aarch64")] {
        extern crate axplat_aarch64_qemu_virt;
        use axplat_aarch64_qemu_virt::config as axplat_config;
    } else if #[cfg(target_arch = "riscv64")] {
        extern crate axplat_riscv64_qemu_virt;
        use axplat_riscv64_qemu_virt::config as axplat_config;
    } else if #[cfg(target_arch = "loongarch64")] {
        extern crate axplat_loongarch64_qemu_virt;
        use axplat_loongarch64_qemu_virt::config as axplat_config;
    } else {
        compile_error!("Unsupported target architecture");
    }
}

mod irq;
use irq::*;

fn init_kernel(cpu_id: usize, arg: usize) {
    // x86_64 requires the `percpu` crate to be initialized first.
    #[cfg(target_arch = "x86_64")]
    axcpu::init::init_percpu(cpu_id);

    // Initialize trap, console, time.
    axplat::init::init_early(cpu_id, arg);

    // Initialize platform peripherals, such as IRQ handlers.
    axplat::init::init_later(cpu_id, arg);
}

#[axplat::main]
fn main(cpu_id: usize, arg: usize) -> ! {
    init_kernel(cpu_id, arg);

    axplat::console_println!("Hello, ArceOS!");
    axplat::console_println!("cpu_id = {cpu_id}, arg = {arg:#x}");

    init_irq();
    test_irq();

    axplat::power::system_off();
}

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    axplat::console_println!("{info}");
    axplat::power::system_off()
}
