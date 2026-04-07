#[cfg(feature = "ax-std")]
use std::os::arceos::modules::axhal;

pub fn assert_irq_enabled() {
    #[cfg(feature = "ax-std")]
    {
        assert!(
            axhal::asm::irqs_enabled(),
            "Task id = {:?} IRQs should be enabled!",
            std::thread::current().id()
        );
    }
}

pub fn assert_irq_disabled() {
    #[cfg(feature = "ax-std")]
    {
        assert!(
            !axhal::asm::irqs_enabled(),
            "Task id = {:?} IRQs should be disabled!",
            std::thread::current().id()
        );
    }
}

pub fn assert_irq_enabled_and_disabled() {
    assert_irq_enabled();
    disable_irqs();
    assert_irq_disabled();
    enable_irqs();
}

pub fn disable_irqs() {
    #[cfg(feature = "ax-std")]
    axhal::asm::disable_irqs()
}

pub fn enable_irqs() {
    #[cfg(feature = "ax-std")]
    axhal::asm::enable_irqs()
}
