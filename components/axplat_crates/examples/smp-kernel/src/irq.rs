use ax_cpu::trap::{IRQ, register_trap_handler};

#[register_trap_handler(IRQ)]
fn irq_handler(vector: usize) -> bool {
    ax_plat::irq::handle(vector);
    true
}

pub fn init_irq() {
    fn update_timer() {
        // One timer interrupt per second.
        static PERIODIC_INTERVAL_NANOS: u64 = ax_plat::time::NANOS_PER_SEC;
        // Reset the timer for the next interrupt.
        #[ax_percpu::def_percpu]
        static NEXT_DEADLINE: u64 = 0;

        ax_plat::console_println!(
            "{:?} elapsed. Timer IRQ processed on CPU {}.",
            ax_plat::time::monotonic_time(),
            ax_plat::percpu::this_cpu_id()
        );

        let now_ns = ax_plat::time::monotonic_time_nanos();
        let mut deadline = unsafe { NEXT_DEADLINE.read_current_raw() };
        if now_ns >= deadline {
            deadline = now_ns + PERIODIC_INTERVAL_NANOS;
        }
        unsafe {
            NEXT_DEADLINE.write_current_raw(deadline + PERIODIC_INTERVAL_NANOS);
        }
        ax_plat::time::set_oneshot_timer(deadline);
    }

    // Register the timer IRQ handler.
    ax_plat::irq::register(axplat_crate::config::devices::TIMER_IRQ, update_timer);
    ax_plat::console_println!("Timer IRQ handler registered.");

    // Enable the timer IRQ.
    ax_cpu::asm::enable_irqs();
}
