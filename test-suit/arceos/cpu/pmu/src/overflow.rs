use core::sync::atomic::{AtomicUsize, Ordering};
use std::{
    os::arceos::modules::ax_hal,
    time::{Duration, Instant},
};

const MAX_CPUS: usize = 64;
static IRQS: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
static PCS: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
static SPS: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
static FPS: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

fn handle(_: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    let cpu = ax_hal::percpu::this_cpu_id();
    assert!(!ax_cpu::interrupt::irqs_enabled());
    // SAFETY: native IRQ entry excludes local reentry; the test owns this PMU
    // and has dropped its process-context register session before enabling IRQs.
    let mut pmu = unsafe { ax_cpu::pmu::Pmu::current() }.unwrap();
    let bit = 1u64 << ax_cpu::pmu::CounterId::CYCLE.index();
    if pmu.overflow_status() & bit == 0 {
        return ax_hal::irq::IrqReturn::Unhandled;
    }
    pmu.disable(ax_cpu::pmu::CounterId::CYCLE).unwrap();
    pmu.disable_overflow_irq(ax_cpu::pmu::CounterId::CYCLE)
        .unwrap();
    assert_eq!(pmu.overflow_status() & bit, 0);
    let snapshot = ax_hal::irq::interrupted_context().expect("native IRQ snapshot");
    assert_eq!(
        snapshot.privilege,
        ax_cpu::trap::InterruptedPrivilege::Kernel
    );
    assert_ne!(snapshot.pc, 0);
    assert_ne!(snapshot.sp, 0);
    PCS[cpu].store(snapshot.pc, Ordering::Relaxed);
    SPS[cpu].store(snapshot.sp, Ordering::Relaxed);
    FPS[cpu].store(snapshot.fp, Ordering::Relaxed);
    IRQS[cpu].fetch_add(1, Ordering::Release);
    ax_hal::irq::IrqReturn::Handled
}

pub fn run(cpus: usize) {
    assert!(cpus <= MAX_CPUS);
    let irq = ax_hal::pmu::irq().expect("firmware PMU PPI");
    let handle = ax_hal::irq::request_percpu_irq(irq, ax_hal::irq::CpuMask::first_n(cpus), handle)
        .expect("exclusive PMU IRQ registration");
    for cpu in 0..cpus {
        super::pin_to(cpu);
        for expected in 1..=2 {
            ax_cpu::interrupt::disable_irqs();
            // SAFETY: this affine task owns the stopped local cycle counter;
            // IRQs are masked until the bounded configuration session ends.
            unsafe {
                let mut pmu = ax_cpu::pmu::Pmu::current().unwrap();
                pmu.preload(ax_cpu::pmu::CounterId::CYCLE, 100_000).unwrap();
                pmu.enable_overflow_irq(ax_cpu::pmu::CounterId::CYCLE)
                    .unwrap();
                pmu.start();
                pmu.enable(ax_cpu::pmu::CounterId::CYCLE).unwrap();
            }
            ax_cpu::interrupt::enable_irqs();
            let started = Instant::now();
            while IRQS[cpu].load(Ordering::Acquire) < expected {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "PMU PPI did not arrive on owner CPU {cpu}"
                );
                core::hint::spin_loop();
            }
            assert_eq!(IRQS[cpu].load(Ordering::Acquire), expected);
        }
        std::println!(
            "CPU_PMU_IRQ cpu={cpu} count=2 pc={:#x} sp={:#x} fp={:#x}",
            PCS[cpu].load(Ordering::Relaxed),
            SPS[cpu].load(Ordering::Relaxed),
            FPS[cpu].load(Ordering::Relaxed)
        );
    }
    ax_hal::irq::disable_irq(handle).unwrap();
    ax_hal::irq::synchronize_irq(handle).unwrap();
    ax_hal::irq::free_irq(handle).unwrap();
    // Every source was disabled and acknowledged before callback retirement.
    std::thread::sleep(Duration::from_millis(10));
    for count in &IRQS[..cpus] {
        assert_eq!(count.load(Ordering::Acquire), 2);
    }
}
