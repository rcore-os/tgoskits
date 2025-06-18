#![no_std]
#![no_main]

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        extern crate axplat_x86_pc;
    } else if #[cfg(target_arch = "aarch64")] {
        extern crate axplat_aarch64_qemu_virt;
    } else if #[cfg(target_arch = "riscv64")] {
        extern crate axplat_riscv64_qemu_virt;
    } else if #[cfg(target_arch = "loongarch64")] {
        extern crate axplat_loongarch64_qemu_virt;
    } else {
        compile_error!("Unsupported target architecture");
    }
}

fn init_kernel(cpu_id: usize, arg: usize) {
    // x86_64 requires the `percpu` crate to be initialized first
    #[cfg(target_arch = "x86_64")]
    axcpu::init::init_percpu(cpu_id);

    // Initialize trap, console, time.
    axplat::init::init_early(cpu_id, arg);

    // Initialize platform devices (not used in this example).
    axplat::init::init_later(cpu_id, arg);
}

#[axplat::main]
fn main(cpu_id: usize, arg: usize) -> ! {
    init_kernel(cpu_id, arg);

    axplat::console_println!("Hello, ArceOS!");
    axplat::console_println!("cpu_id = {cpu_id}, arg = {arg:#x}");

    for _ in 0..5 {
        axplat::time::busy_wait(axplat::time::TimeValue::from_secs(1));
        axplat::console_println!("{:?} elapsed.", axplat::time::monotonic_time());
    }

    axplat::console_println!("All done, shutting down!");
    axplat::power::system_off();
}

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    axplat::console_println!("{info}");
    axplat::power::system_off()
}
