#![no_std]
#![no_main]

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        extern crate axplat_x86_pc as axplat_crate;
    } else if #[cfg(target_arch = "aarch64")] {
        extern crate axplat_aarch64_qemu_virt as axplat_crate;
    } else if #[cfg(target_arch = "riscv64")] {
        extern crate axplat_riscv64_qemu_virt as axplat_crate;
    } else if #[cfg(target_arch = "loongarch64")] {
        extern crate axplat_loongarch64_qemu_virt as axplat_crate;
    } else {
        compile_error!("Unsupported target architecture");
    }
}

mod init;
mod irq;
mod mp;

use init::*;
use irq::*;
use mp::start_secondary_cpus;

use core::sync::atomic::Ordering::Release;

const CPU_NUM: usize = match option_env!("AX_CPU_NUM") {
    Some(val) => const_str::parse!(val, usize),
    None => axplat_crate::config::plat::MAX_CPU_NUM,
};

#[axplat::main]
fn main(cpu_id: usize, arg: usize) -> ! {
    init_kernel(cpu_id, arg);

    axplat::console_println!("Hello, ArceOS!");
    axplat::console_println!("Primary CPU {cpu_id} started.");

    start_secondary_cpus(cpu_id);

    init_irq();

    INITED_CPUS.fetch_add(1, Release);

    axplat::console_println!("Primary CPU {cpu_id} init OK.");

    while !init_smp_ok() {
        core::hint::spin_loop();
    }

    axplat::time::busy_wait(axplat::time::TimeValue::from_secs(5));

    axplat::console_println!("Primary CPU {cpu_id} finished. Shutting down...");

    axplat::power::system_off();
}

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    axplat::console_println!("{info}");
    axplat::power::system_off()
}
