use core::{arch::asm, hint::black_box, ptr};
use std::{println, thread};

use ax_std::os::arceos::{
    api::{
        stdio::ax_console_flush,
        task::{AxCpuMask, ax_set_current_affinity},
    },
    modules::ax_hal::{cpu::interrupt::disable_irqs, percpu::this_cpu_id},
};

// These QEMU profiles assign the runtime console to the boot CPU.
const CONSOLE_OWNER_CPU: usize = 0;
const STACK_SIZE: usize = 64 * 1024;
const WRITE_STRIDE: usize = 64;
const SEARCH_BYTES: usize = STACK_SIZE + 2 * 4096;

pub fn run() -> crate::TestResult {
    let cpu_num = ax_std::os::arceos::task::sched::cpu_topology_len().unwrap();
    // Keep cross-CPU stack creation and migration, but fault on the UART owner
    // so local IRQ exclusion also excludes every normal register transaction.
    pin_current_to_cpu(if cpu_num > 1 { 1 } else { CONSOLE_OWNER_CPU });
    let creator_cpu = this_cpu_id();
    let target_cpu = CONSOLE_OWNER_CPU;
    println!(
        "Triggering task stack guard page: creator_cpu={creator_cpu}, target_cpu={target_cpu}, \
         cpu_num={cpu_num}"
    );

    ax_std::os::arceos::thread::builder("stack-guard-page-overflow".into())
        .stack_size(STACK_SIZE)
        .spawn(move || hit_guard_page(target_cpu))
        .expect("failed to spawn the kernel stack guard probe");

    loop {
        thread::yield_now();
    }
}

#[inline(never)]
fn hit_guard_page(target_cpu: usize) {
    pin_current_to_cpu(target_cpu);

    let sp = current_stack_pointer();
    println!("stack guard probe cpu={} sp={:#x}", this_cpu_id(), sp);
    ax_console_flush().expect("drain output before the terminal stack guard probe");
    // No normal UART task or IRQ can own its register gate on this CPU now.
    // The synchronous guard fault remains deliverable with local IRQs masked.
    disable_irqs();

    let mut offset = WRITE_STRIDE;
    while offset <= SEARCH_BYTES {
        let addr = sp.wrapping_sub(offset) as *mut u8;
        unsafe {
            ptr::write_volatile(addr, offset as u8);
        }
        black_box(addr);
        offset += WRITE_STRIDE;
    }

    panic!("stack guard page was not hit");
}

fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin stack guard probe to CPU {cpu_id}"
    );

    for _ in 0..256 {
        if this_cpu_id() == cpu_id {
            return;
        }
        thread::yield_now();
    }

    assert_eq!(
        this_cpu_id(),
        cpu_id,
        "stack guard probe did not migrate to CPU {cpu_id}"
    );
}

#[inline(always)]
fn current_stack_pointer() -> usize {
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("mv {}, sp", out(reg) sp, options(nomem, nostack));

        #[cfg(target_arch = "x86_64")]
        asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack));

        #[cfg(target_arch = "aarch64")]
        asm!("mov {}, sp", out(reg) sp, options(nomem, nostack));

        #[cfg(target_arch = "loongarch64")]
        asm!("move {}, $sp", out(reg) sp, options(nomem, nostack));
    }
    sp
}
