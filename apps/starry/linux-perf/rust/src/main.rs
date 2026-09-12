use core::{arch::global_asm, mem::size_of};
use std::io;

global_asm!(
    r#"
    .text
    .align 2
    .global perf_level_one
    .type perf_level_one, %function
perf_level_one:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    bl perf_level_two
    ldp x29, x30, [sp], #16
    ret

    .global perf_level_two
    .type perf_level_two, %function
perf_level_two:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    bl perf_level_three
    ldp x29, x30, [sp], #16
    ret

    .global perf_level_three
    .type perf_level_three, %function
perf_level_three:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    bl perf_leaf
    ldp x29, x30, [sp], #16
    ret

    .global perf_leaf
    .type perf_leaf, %function
perf_leaf:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    mov x1, x0
1:
    subs x1, x1, #1
    b.ne 1b
    ldp x29, x30, [sp], #16
    ret
"#
);

unsafe extern "C" {
    fn perf_level_one(iterations: u64);
    fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const CpuSet) -> i32;
}

#[repr(C)]
struct CpuSet {
    bits: [usize; 16],
}

fn pin_to_cpu(cpu: usize) -> io::Result<()> {
    let mut mask = CpuSet { bits: [0; 16] };
    let word_bits = usize::BITS as usize;
    mask.bits[cpu / word_bits] |= 1usize << (cpu % word_bits);
    // SAFETY: `mask` has musl's 128-byte cpu_set_t layout on AArch64 and stays
    // live for the duration of the call. pid 0 selects the calling thread.
    if unsafe { sched_setaffinity(0, size_of::<CpuSet>(), &mask) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn run(iterations: u64) {
    // SAFETY: the assembly routine follows AAPCS64 and preserves its frame
    // pointer chain specifically so perf can unwind the user callchain.
    unsafe { perf_level_one(iterations) };
}

fn main() {
    println!("STARRY_LINUX_PERF_WORKLOAD_BEGIN");
    if std::env::args().any(|argument| argument == "--migrate") {
        pin_to_cpu(0).expect("pin workload to CPU 0");
        run(50_000_000);
        pin_to_cpu(4).expect("migrate workload to CPU 4");
        run(50_000_000);
        println!("STARRY_LINUX_PERF_WORKLOAD_MIGRATED");
    } else {
        run(100_000_000);
    }
    println!("STARRY_LINUX_PERF_WORKLOAD_END");
}
