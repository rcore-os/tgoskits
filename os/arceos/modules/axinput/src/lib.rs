//! [ArceOS](https://github.com/arceos-org/arceos) input module.

#![no_std]

extern crate alloc;

mod device;
mod event;
mod id;
pub mod rdif;

use alloc::vec::Vec;
use core::mem;

use ax_lazyinit::LazyInit;
use ax_sync::SpinMutex;
pub use device::{ErasedInputDevice, InputDevice, InputError, InputIrqEvent, InputResult};
pub use event::{AbsInfo, Event, EventType};
pub use id::InputDeviceId;

static DEVICES: LazyInit<SpinMutex<Vec<ErasedInputDevice>>> = LazyInit::new();

/// Initializes the input subsystem by underlayer devices.
pub fn init_input(input_devs: impl IntoIterator<Item = ErasedInputDevice>) {
    log::info!("Initialize input subsystem...");

    let mut devices = Vec::new();
    for dev in input_devs {
        log::info!("  registered a new input device: {}", dev.name());
        devices.push(dev);
    }
    DEVICES.init_once(SpinMutex::new(devices));
}

/// Takes the initialized input devices.
pub fn take_inputs() -> Vec<ErasedInputDevice> {
    mem::take(&mut DEVICES.lock())
}

/// Returns whether an evdev polling fallback should actively drain the device
/// queue.
pub fn input_polling_fallback_should_drain(
    polling_requested: bool,
    now_ns: u64,
    last_irq_event_ns: u64,
    irq_alive_ns: u64,
) -> bool {
    polling_requested && now_ns.wrapping_sub(last_irq_event_ns) > irq_alive_ns
}

#[cfg(test)]
mod tests {
    use super::input_polling_fallback_should_drain;

    const IRQ_ALIVE_NS: u64 = 1_000_000_000;

    #[test]
    fn polling_fallback_stays_idle_without_user_interest_even_when_irq_is_stale() {
        assert!(!input_polling_fallback_should_drain(
            false,
            IRQ_ALIVE_NS + 1,
            0,
            IRQ_ALIVE_NS
        ));
    }

    #[test]
    fn polling_fallback_drains_after_user_interest_when_irq_is_stale() {
        assert!(input_polling_fallback_should_drain(
            true,
            IRQ_ALIVE_NS + 1,
            0,
            IRQ_ALIVE_NS
        ));
    }

    #[test]
    fn polling_fallback_stays_idle_while_irq_is_recent() {
        assert!(!input_polling_fallback_should_drain(
            true,
            IRQ_ALIVE_NS,
            0,
            IRQ_ALIVE_NS
        ));
    }
}
