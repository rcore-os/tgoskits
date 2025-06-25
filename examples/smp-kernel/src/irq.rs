use axcpu::trap::{IRQ, register_trap_handler};

#[register_trap_handler(IRQ)]
fn irq_handler(vector: usize) -> bool {
    axplat::irq::handle(vector);
    true
}

pub fn init_irq() {
    fn update_timer() {
        // One timer interrupt per second.
        static PERIODIC_INTERVAL_NANOS: u64 = axplat::time::NANOS_PER_SEC;
        // Reset the timer for the next interrupt.
        #[percpu::def_percpu]
        static NEXT_DEADLINE: u64 = 0;

        axplat::console_println!(
            "{:?} elapsed. Timer IRQ processed on CPU {}.",
            axplat::time::monotonic_time(),
            crate::this_cpu_id()
        );

        let now_ns = axplat::time::monotonic_time_nanos();
        let mut deadline = unsafe { NEXT_DEADLINE.read_current_raw() };
        if now_ns >= deadline {
            deadline = now_ns + PERIODIC_INTERVAL_NANOS;
        }
        unsafe {
            NEXT_DEADLINE.write_current_raw(deadline + PERIODIC_INTERVAL_NANOS);
        }
        axplat::time::set_oneshot_timer(deadline);
    }

    // Register the timer IRQ handler.
    axplat::irq::register(crate::axplat_config::devices::TIMER_IRQ, update_timer);
    axplat::console_println!("Timer IRQ handler registered.");

    // Enable the timer IRQ.
    axplat::irq::set_enable(crate::axplat_config::devices::TIMER_IRQ, true);
    axcpu::asm::enable_irqs();
}
