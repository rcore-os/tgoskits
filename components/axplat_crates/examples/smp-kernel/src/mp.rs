use core::sync::atomic::Ordering::{Acquire, Release};
use memory_addr::VirtAddr;

use crate::{CPU_NUM, INITED_CPUS, init_kernel_secondary};
use axplat_crate::config::plat::BOOT_STACK_SIZE;

#[unsafe(link_section = ".bss.stack")]
static mut SECONDARY_BOOT_STACK: [[u8; BOOT_STACK_SIZE]; CPU_NUM - 1] =
    [[0; BOOT_STACK_SIZE]; CPU_NUM - 1];

#[allow(clippy::absurd_extreme_comparisons)]
pub fn start_secondary_cpus(primary_cpu_id: usize) {
    let mut logic_cpu_id = 0;
    for i in 0..CPU_NUM {
        if i != primary_cpu_id && logic_cpu_id < CPU_NUM - 1 {
            let stack_top = axplat::mem::virt_to_phys(VirtAddr::from(unsafe {
                SECONDARY_BOOT_STACK[logic_cpu_id].as_ptr_range().end as usize
            }));

            axplat::power::cpu_boot(i, stack_top.as_usize());

            logic_cpu_id += 1;

            while INITED_CPUS.load(Acquire) < logic_cpu_id {
                core::hint::spin_loop();
            }
        }
    }
}

#[axplat::secondary_main]
fn secondary_main(cpu_id: usize) -> ! {
    init_kernel_secondary(cpu_id);

    INITED_CPUS.fetch_add(1, Release);

    axplat::console_println!("Secondary CPU {cpu_id} init OK.");

    while !crate::init_smp_ok() {
        core::hint::spin_loop();
    }

    axcpu::asm::enable_irqs();

    // Infinite loop to receive and handle timer interrupts
    loop {
        core::hint::spin_loop();
    }
}
