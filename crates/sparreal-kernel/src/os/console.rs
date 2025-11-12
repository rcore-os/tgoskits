use core::fmt::{self, Write};

use crate::hal;

pub trait Console {
    fn write_fmt(&self, args: fmt::Arguments<'_>);
    fn read(&self) -> Option<u8>;
}

static mut CON: &dyn Console = &EarlyConsole;

fn con() -> &'static dyn Console {
    unsafe { CON }
}

struct EarlyConsole;

impl Console for EarlyConsole {
    fn write_fmt(&self, args: fmt::Arguments<'_>) {
        let mut writer = EarlyConsole;
        Write::write_fmt(&mut writer, args).unwrap();
    }
    fn read(&self) -> Option<u8> {
        hal::al::console::early_read()
    }
}

impl Write for EarlyConsole {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            let n = hal::al::console::early_write(bytes);
            bytes = &bytes[n..];
        }
        Ok(())
    }
}

pub fn _write_fmt(args: fmt::Arguments<'_>) {
    con().write_fmt(args);
}
