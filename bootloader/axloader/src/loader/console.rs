use core::fmt;

use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams};
use uefi::proto::console::serial::{IoMode, Parity, Serial, StopBits};
use uefi::proto::console::text::Key;

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        $crate::console::serial_print(format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {{
        $crate::console::serial_println(format_args!($($arg)*));
    }};
}

pub fn serial_print(args: fmt::Arguments<'_>) {
    let _ = uefi::system::with_stdout(|stdout| fmt::write(stdout, args));
    let _ = with_serial(|serial| {
        let mut writer = SerialWriter { serial };
        fmt::write(&mut writer, args)
    });
}

pub fn serial_println(args: fmt::Arguments<'_>) {
    serial_print(args);
    serial_print(format_args!("\n"));
}

pub fn serial_read_byte() -> Option<u8> {
    if let Some(byte) = read_serial_byte() {
        return Some(byte);
    }

    uefi::system::with_stdin(|stdin| match stdin.read_key() {
        Ok(Some(Key::Printable(ch))) => {
            let ch = char::from(ch);
            if ch.is_ascii() {
                Some(ch as u8)
            } else {
                None
            }
        }
        Ok(Some(Key::Special(_))) | Ok(None) | Err(_) => None,
    })
}

fn with_serial<R>(f: impl FnOnce(&mut Serial) -> R) -> Option<R> {
    let handles = boot::find_handles::<Serial>().ok()?;
    for handle in handles {
        let protocol = unsafe {
            boot::open_protocol::<Serial>(
                OpenProtocolParams {
                    handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        };
        let Ok(mut serial) = protocol else {
            continue;
        };

        configure_serial(&mut serial);
        return Some(f(&mut serial));
    }
    None
}

fn configure_serial(serial: &mut Serial) {
    let mode = IoMode {
        control_mask: serial.io_mode().control_mask,
        timeout: 1_000,
        baud_rate: 115_200,
        receive_fifo_depth: 0,
        data_bits: 8,
        parity: Parity::NONE,
        stop_bits: StopBits::ONE,
    };
    let _ = serial.set_attributes(&mode);
}

fn read_serial_byte() -> Option<u8> {
    with_serial(|serial| {
        let mut byte = [0];
        serial.read(&mut byte).ok().map(|()| byte[0])
    })
    .flatten()
}

struct SerialWriter<'a> {
    serial: &'a mut Serial,
}

impl fmt::Write for SerialWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut start = 0;
        for (index, byte) in text.bytes().enumerate() {
            if byte != b'\n' {
                continue;
            }
            if start < index {
                self.serial
                    .write_exact(&text.as_bytes()[start..index])
                    .map_err(|_| fmt::Error)?;
            }
            self.serial.write_exact(b"\r\n").map_err(|_| fmt::Error)?;
            start = index + 1;
        }
        if start < text.len() {
            self.serial
                .write_exact(&text.as_bytes()[start..])
                .map_err(|_| fmt::Error)?;
        }
        Ok(())
    }
}
