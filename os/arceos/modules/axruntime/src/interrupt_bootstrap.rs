//! Per-CPU runtime IRQ registration.

pub(crate) fn init_current_cpu() {
    init_cpu(ax_hal::percpu::this_cpu_id());
}

pub(crate) fn init_cpu(cpu_id: usize) {
    ax_hal::irq::cpu_online(cpu_id).expect("failed to mark CPU online for IRQ framework");
    ax_hal::irq::init_common_irq_handler();

    if ax_hal::percpu::this_cpu_is_bsp() {
        let cpus = ax_hal::irq::CpuMask::first_n(ax_hal::cpu_num());
        ax_hal::irq::request_percpu_irq(
            ax_hal::time::irq_num(),
            cpus,
            crate::clock_event_runtime::timer_irq_handler,
        )
        .expect("failed to register timer IRQ handler");

        #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
        ax_hal::irq::request_percpu_irq(
            ax_hal::irq::ipi_irq(),
            cpus,
            crate::ipi_delivery::irq_handler,
        )
        .expect("failed to register IPI IRQ handler");
    }

    #[cfg(not(feature = "multitask"))]
    crate::clock_event_runtime::init_timer();
}
