use core::fmt;

use uefi::{
    Status,
    boot::{self, OpenProtocolAttributes, OpenProtocolParams},
    proto::console::{
        serial::{IoMode, Parity, Serial, StopBits},
        text::Key,
    },
};

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

pub fn serial_read_available(buffer: &mut [u8]) -> usize {
    if buffer.is_empty() {
        return 0;
    }
    if let Some(read) = read_serial_available(buffer) {
        return read;
    }

    uefi::system::with_stdin(|stdin| match stdin.read_key() {
        Ok(Some(Key::Printable(ch))) => {
            let ch = char::from(ch);
            if ch.is_ascii() {
                buffer[0] = ch as u8;
                1
            } else {
                0
            }
        }
        Ok(Some(Key::Special(_))) | Ok(None) | Err(_) => 0,
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

fn read_serial_available(buffer: &mut [u8]) -> Option<usize> {
    with_serial(|serial| match serial.read(buffer) {
        Ok(()) => buffer.len(),
        Err(error) if error.status() == Status::TIMEOUT => *error.data(),
        Err(_) => 0,
    })
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
