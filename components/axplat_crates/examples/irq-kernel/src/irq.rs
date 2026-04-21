use core::sync::atomic::{
    AtomicU64,
    Ordering::{Acquire, Release},
};

use ax_cpu::trap::irq_handler;

const TICKS_PER_SEC: u64 = 100;

static IRQ_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn irq_count() -> u64 {
    IRQ_COUNTER.load(Acquire)
}

#[irq_handler]
fn handle_irq(vector: usize) -> bool {
    ax_plat::irq::handle(vector);
    true
}

pub fn init_irq() {
    fn update_timer(_irq_num: usize) {
        static PERIODIC_INTERVAL_NANOS: u64 = ax_plat::time::NANOS_PER_SEC / TICKS_PER_SEC;

        IRQ_COUNTER.fetch_add(1, Release);
        // Reset the timer for the next interrupt.
        static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(0);

        let now_ns = ax_plat::time::monotonic_time_nanos();
        let mut deadline = NEXT_DEADLINE.load(Acquire);
        if now_ns >= deadline {
            deadline = now_ns + PERIODIC_INTERVAL_NANOS;
        }

        NEXT_DEADLINE.store(deadline + PERIODIC_INTERVAL_NANOS, Release);
        ax_plat::time::set_oneshot_timer(deadline);
    }

    // Register the timer IRQ handler.
    ax_plat::irq::register(axplat_crate::config::devices::TIMER_IRQ, update_timer);
    ax_plat::console_println!("Timer IRQ handler registered.");

    // Enable the timer IRQ.
    ax_cpu::asm::enable_irqs();
}

pub fn test_irq() {
    let interval = 5;
    ax_plat::console_println!("Waiting for timer IRQs for {} seconds...", interval);

    for _ in 0..interval {
        ax_plat::time::busy_wait(ax_plat::time::TimeValue::from_secs(1));
        ax_plat::console_println!(
            "{:?} elapsed. {} Timer IRQ processed.",
            ax_plat::time::monotonic_time(),
            irq_count()
        );
    }

    let irq_count = irq_count();
    ax_plat::console_println!("Timer IRQ count: {irq_count}");

    // A lower bound for the number of IRQs expected in the given interval.
    let irq_min_count = TICKS_PER_SEC * interval;

    if irq_count < irq_min_count {
        panic!(
            "Timer IRQ was not triggered enough times within the expected time frame, expected at \
             least {irq_min_count}, got {irq_count}"
        );
    }

    ax_plat::console_println!("Timer IRQ test passed.");
}
