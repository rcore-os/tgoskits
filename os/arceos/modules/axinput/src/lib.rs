//! [ArceOS](https://github.com/arceos-org/arceos) input module.

#![no_std]

extern crate alloc;

mod device;
mod event;
mod id;

use alloc::vec::Vec;
use core::mem;

use ax_lazyinit::LazyInit;
use ax_sync::SpinMutex;
pub use device::*;
pub use event::{AbsInfo, Event, EventType};
pub use id::InputDeviceId;

static DEVICES: LazyInit<SpinMutex<Vec<InputDeviceFacade>>> = LazyInit::new();

/// Initializes the input subsystem by underlayer devices.
pub fn init_input(input_devs: impl IntoIterator<Item = InputDeviceFacade>) {
    log::info!("Initialize input subsystem...");

    let mut devices = Vec::new();
    for dev in input_devs {
        log::info!("  registered a new input device: {}", dev.snapshot().name());
        devices.push(dev);
    }
    DEVICES.init_once(SpinMutex::new(devices));
}

/// Takes the initialized input devices.
pub fn take_inputs() -> Vec<InputDeviceFacade> {
    mem::take(&mut DEVICES.lock())
}
