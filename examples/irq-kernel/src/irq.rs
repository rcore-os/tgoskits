use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering::{Acquire, Release};

use axcpu::trap::{IRQ, register_trap_handler};

const TICKS_PER_SEC: u64 = 100;

static IRQ_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn irq_count() -> u64 {
    IRQ_COUNTER.load(Acquire)
}

#[register_trap_handler(IRQ)]
fn irq_handler(vector: usize) -> bool {
    axplat::irq::handle(vector);
    true
}

pub fn init_irq() {
    fn update_timer() {
        static PERIODIC_INTERVAL_NANOS: u64 = axplat::time::NANOS_PER_SEC / TICKS_PER_SEC;

        IRQ_COUNTER.fetch_add(1, Release);
        // Reset the timer for the next interrupt.
        static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(0);

        let now_ns = axplat::time::monotonic_time_nanos();
        let mut deadline = NEXT_DEADLINE.load(Acquire);
        if now_ns >= deadline {
            deadline = now_ns + PERIODIC_INTERVAL_NANOS;
        }

        NEXT_DEADLINE.store(deadline + PERIODIC_INTERVAL_NANOS, Release);
        axplat::time::set_oneshot_timer(deadline);
    }

    // Register the timer IRQ handler.
    axplat::irq::register(crate::axplat_config::devices::TIMER_IRQ, update_timer);
    axplat::console_println!("Timer IRQ handler registered.");

    // Enable the timer IRQ.
    axplat::irq::set_enable(crate::axplat_config::devices::TIMER_IRQ, true);
    axcpu::asm::enable_irqs();
}

pub fn test_irq() {
    axplat::console_println!("Waiting for timer IRQs for 5 seconds...",);

    for _ in 0..5 {
        axplat::time::busy_wait(axplat::time::TimeValue::from_secs(1));
        axplat::console_println!(
            "{:?} elapsed. {} Timer IRQ processed.",
            axplat::time::monotonic_time(),
            irq_count()
        );
    }

    let irq_count = irq_count();
    axplat::console_println!("Timer IRQ count: {irq_count}");

    // A lower bound for the number of IRQs expected in the given interval.
    let irq_min_count = TICKS_PER_SEC * 5;

    if irq_count < irq_min_count {
        panic!(
            "Timer IRQ was not triggered enough times within the expected time frame, expected at least {irq_min_count}, got {irq_count}"
        );
    }

    axplat::console_println!("Timer IRQ test passed.");
}
