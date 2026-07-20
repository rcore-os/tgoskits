use core::{arch::asm, hint::black_box, ptr};
use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::ax_hal::percpu::this_cpu_id,
    },
    println, thread,
};

const STACK_SIZE: usize = 64 * 1024;
const WRITE_STRIDE: usize = 64;
const SEARCH_BYTES: usize = STACK_SIZE + 2 * 4096;

pub fn run() -> crate::TestResult {
    let cpu_num = thread::available_parallelism().unwrap().get();
    let creator_cpu = this_cpu_id();
    let target_cpu = if cpu_num > 1 {
        (creator_cpu + 1) % cpu_num
    } else {
        creator_cpu
    };
    println!(
        "Triggering task stack guard page: creator_cpu={creator_cpu}, target_cpu={target_cpu}, \
         cpu_num={cpu_num}"
    );

    let _ = thread::Builder::new()
        .name("stack-guard-page-overflow".into())
        .stack_size(STACK_SIZE)
        .spawn(move || hit_guard_page(target_cpu));

    loop {
        thread::yield_now();
    }
}

#[inline(never)]
fn hit_guard_page(target_cpu: usize) {
    pin_current_to_cpu(target_cpu);

    let sp = current_stack_pointer();
    println!("stack guard probe cpu={} sp={:#x}", this_cpu_id(), sp);

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
