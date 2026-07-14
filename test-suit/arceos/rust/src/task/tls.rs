#![allow(unused_unsafe)]

#[cfg(target_arch = "riscv64")]
use core::sync::atomic::{AtomicBool, Ordering};
use std::{
    os::arceos::{
        modules::ax_hal,
        task::{CpuId, CpuSet, set_current_thread_affinity},
    },
    ptr::addr_of,
    str::from_utf8_unchecked,
    thread,
    time::Duration,
    vec::Vec,
};

#[thread_local]
static mut BOOL: bool = true;
#[thread_local]
static mut U8: u8 = 0xAA;
#[thread_local]
static mut U16: u16 = 0xcafe;
#[thread_local]
static mut U32: u32 = 0xdeadbeed;
#[thread_local]
static mut U64: u64 = 0xa2ce05_a2ce05;
#[thread_local]
static mut STR: [u8; 13] = *b"Hello, world!";

const STR_LEN: usize = 13;

macro_rules! get {
    ($var:expr) => {
        unsafe { $var }
    };
}

macro_rules! set {
    ($var:expr, $value:expr) => {
        unsafe { $var = $value }
    };
}

macro_rules! add {
    ($var:expr, $value:expr) => {
        unsafe { $var += $value }
    };
}

fn assert_thread_local_values(task_index: usize) {
    assert_eq!(get!(BOOL), task_index.is_multiple_of(2));
    assert_eq!(get!(U8), 0xAA + task_index as u8);
    assert_eq!(get!(U16), 0xcafe + task_index as u16);
    assert_eq!(get!(U32), 0xdeadbeed + task_index as u32);
    assert_eq!(get!(U64), 0xa2ce05_a2ce05 + task_index as u64);
    assert_eq!(get!(STR[5]), 48 + task_index as u8);
    assert_eq!(STR_LEN, 13);
    let _ = get!(from_utf8_unchecked(&*addr_of!(STR)));
}

fn exercise_tls_across_scheduler_paths(task_index: usize, cpu_count: usize) {
    let source_cpu = CpuId::new(0);
    let mut source_only = CpuSet::empty(cpu_count);
    assert!(source_only.insert(source_cpu));
    set_current_thread_affinity(source_only).expect("failed to pin TLS task to CPU0");
    assert_eq!(ax_hal::percpu::this_cpu_id(), source_cpu.as_usize());

    set!(BOOL, task_index.is_multiple_of(2));
    add!(U8, task_index as u8);
    add!(U16, task_index as u16);
    add!(U32, task_index as u32);
    add!(U64, task_index as u64);
    set!(STR[5], 48 + task_index as u8);

    for _ in 0..4 {
        thread::yield_now();
        thread::sleep(Duration::from_millis(5));
        assert_eq!(ax_hal::percpu::this_cpu_id(), source_cpu.as_usize());
        assert_thread_local_values(task_index);
    }

    if cpu_count > 1 {
        let destination_cpu = CpuId::new(1);
        let mut destination_only = CpuSet::empty(cpu_count);
        assert!(destination_only.insert(destination_cpu));
        set_current_thread_affinity(destination_only).expect("failed to migrate TLS task to CPU1");
        assert_eq!(
            ax_hal::percpu::this_cpu_id(),
            destination_cpu.as_usize(),
            "affinity update returned before TLS task migration completed"
        );

        for _ in 0..4 {
            thread::yield_now();
            thread::sleep(Duration::from_millis(5));
            assert_eq!(ax_hal::percpu::this_cpu_id(), destination_cpu.as_usize());
            assert_thread_local_values(task_index);
        }
    }
}

#[cfg(target_arch = "riscv64")]
fn exercise_bootstrap_fp_off_switch() {
    static RELEASE_CHILD: AtomicBool = AtomicBool::new(false);
    static CHILD_FINISHED: AtomicBool = AtomicBool::new(false);

    RELEASE_CHILD.store(false, Ordering::Relaxed);
    CHILD_FINISHED.store(false, Ordering::Relaxed);
    let child = thread::spawn(|| {
        while !RELEASE_CHILD.load(Ordering::Acquire) {
            thread::yield_now();
        }
        CHILD_FINISHED.store(true, Ordering::Release);
    });

    // Model the bootstrap context handed to the first task. The privileged
    // architecture permits no floating-point instruction while FS is Off, so
    // the context switch must enable FP before clearing the child's registers.
    // SAFETY: this test runs in supervisor mode, changes only the current
    // hart's FS field, and executes integer-only publication before yielding
    // directly into the scheduler.
    unsafe {
        core::arch::asm!(
            "csrc sstatus, {fs_mask}",
            fs_mask = in(reg) 0b11usize << 13,
            options(nomem, nostack),
        );
    }
    RELEASE_CHILD.store(true, Ordering::Release);
    thread::yield_now();

    child.join().unwrap();
    assert!(CHILD_FINISHED.load(Ordering::Acquire));
}

#[cfg(not(target_arch = "riscv64"))]
fn exercise_bootstrap_fp_off_switch() {}

pub fn run() -> crate::TestResult {
    assert!(get!(BOOL));
    assert_eq!(get!(U8), 0xAA);
    assert_eq!(get!(U16), 0xcafe);
    assert_eq!(get!(U32), 0xdeadbeed);
    assert_eq!(get!(U64), 0xa2ce05_a2ce05);
    assert_eq!(get!(&*addr_of!(STR)), b"Hello, world!");

    exercise_bootstrap_fp_off_switch();

    let cpu_count = thread::available_parallelism().unwrap().get();
    let mut tasks = Vec::new();
    for i in 1..=10 {
        tasks.push(thread::spawn(move || {
            exercise_tls_across_scheduler_paths(i, cpu_count);
        }));
    }

    tasks.into_iter().for_each(|task| task.join().unwrap());

    assert!(get!(BOOL));
    assert_eq!(get!(U8), 0xAA);
    assert_eq!(get!(U16), 0xcafe);
    assert_eq!(get!(U32), 0xdeadbeed);
    assert_eq!(get!(U64), 0xa2ce05_a2ce05);
    assert_eq!(get!(&*addr_of!(STR)), b"Hello, world!");
    Ok(())
}
