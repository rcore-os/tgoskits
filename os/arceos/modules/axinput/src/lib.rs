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
use ax_sync::Mutex;
pub use device::{ErasedInputDevice, InputDevice, InputError, InputResult};
pub use event::{AbsInfo, Event, EventType};
pub use id::InputDeviceId;

static DEVICES: LazyInit<Mutex<Vec<ErasedInputDevice>>> = LazyInit::new();

/// Initializes the input subsystem by underlayer devices.
pub fn init_input(input_devs: impl IntoIterator<Item = ErasedInputDevice>) {
    log::info!("Initialize input subsystem...");

    let mut devices = Vec::new();
    for dev in input_devs {
        log::info!("  registered a new input device: {}", dev.name());
        devices.push(dev);
    }
    DEVICES.init_once(Mutex::new(devices));
}

/// Takes the initialized input devices.
pub fn take_inputs() -> Vec<ErasedInputDevice> {
    mem::take(&mut DEVICES.lock())
}
