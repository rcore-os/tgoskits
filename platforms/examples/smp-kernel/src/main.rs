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

mod init;
mod irq;
mod mp;

use core::sync::atomic::Ordering::Release;

use init::*;
use irq::*;
use mp::start_secondary_cpus;

const CPU_NUM: usize = match option_env!("AX_CPU_NUM") {
    Some(val) => const_str::parse!(val, usize),
    None => axplat_crate::config::plat::MAX_CPU_NUM,
};

#[ax_plat::main]
fn main(cpu_id: usize, arg: usize) -> ! {
    init_kernel(cpu_id, arg);

    ax_plat::console_println!("Hello, ArceOS!");
    ax_plat::console_println!("Primary CPU {cpu_id} started.");

    start_secondary_cpus(cpu_id);

    init_irq();

    INITED_CPUS.fetch_add(1, Release);

    ax_plat::console_println!("Primary CPU {cpu_id} init OK.");

    while !init_smp_ok() {
        core::hint::spin_loop();
    }

    ax_plat::time::busy_wait(ax_plat::time::TimeValue::from_secs(5));

    ax_plat::console_println!("Primary CPU {cpu_id} finished. Shutting down...");

    ax_plat::power::system_off();
}

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    ax_plat::console_println!("{info}");
    ax_plat::power::system_off()
}
