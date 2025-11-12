use core::fmt::Write;
use core::{cell::UnsafeCell, ptr::NonNull};
use some_serial::*;

use crate::cmdline::EarlyconConfig;
use crate::mem::phys_to_virt;

pub(crate) static mut DEBUG_BASE: usize = 0;

pub fn _print(args: core::fmt::Arguments) {
    let _ = ConFmt {}.write_fmt(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(core::format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\r\n"));
    ($($arg:tt)*) => ($crate::console::_print(core::format_args!("{}{}", core::format_args!($($arg)*), "\r\n")));
}

macro_rules! pr_range {
    ($name:expr, $b:expr, $s:expr) => {
        $crate::println!(
            "{:<20}: [0x{:0>16x}, 0x{:0>16x}) ({:>5} Mb)",
            $name,
            $b,
            $b + $s,
            ($s) / 1024 / 1024
        );
    };
    ($name:expr, $b:expr, $s:expr, $($arg:tt)*) => {
        $crate::println!(
            "{:<20}: [0x{:0>16x}, 0x{:0>16x}) ({:>5} Mb) {}",
            $name,
            $b,
            $b + $s,
            ($s) / 1024 / 1024,
            core::format_args!($($arg)*)
        );
    };
}

#[allow(dead_code)]
struct ConFmt {}

impl Write for ConFmt {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        con().write_str(s);
        Ok(())
    }
}

#[allow(dead_code)]
fn con() -> &'static dyn Con {
    unsafe { CON }
}

#[allow(dead_code)]
pub(crate) trait Con: Send + Sync {
    fn write_str(&self, s: &str);
}

#[allow(dead_code)]
struct NoCon;
impl Con for NoCon {
    fn write_str(&self, _s: &str) {
        // Do nothing
    }
}

#[allow(dead_code)]
static mut CON: &dyn Con = &NoCon;

#[allow(dead_code)]
pub(crate) unsafe fn set_out(v: &'static dyn Con) {
    unsafe {
        CON = v;
    }
}

pub fn set_earlycon_sender(sender: Sender) {
    unsafe {
        *EARLYCON_SENDER.0.get() = Some(sender);
        set_out(&EARLYCON_SENDER);
    }
}

pub fn set_earlycon_reciever(reciever: Reciever) {
    unsafe {
        *EARLYCON_RECIEVER.0.get() = Some(reciever);
    }
}

#[allow(dead_code)]
#[unsafe(link_section = ".data")]
static EARLYCON_SENDER: EarlyconSenderCell = EarlyconSenderCell(UnsafeCell::new(None));

#[allow(dead_code)]
struct EarlyconSenderCell(UnsafeCell<Option<Sender>>);

unsafe impl Sync for EarlyconSenderCell {}

impl Con for EarlyconSenderCell {
    fn write_str(&self, s: &str) {
        unsafe {
            if let Some(ref mut sender) = *self.0.get() {
                let bytes = s.as_bytes();
                let mut buff = bytes;
                while !buff.is_empty() {
                    let n = sender.write_bytes(buff);
                    buff = &buff[n..];
                }
            }
        }
    }
}

#[unsafe(link_section = ".data")]
static EARLYCON_RECIEVER: EarlyconRecieverCell = EarlyconRecieverCell(UnsafeCell::new(None));

#[allow(dead_code)]
struct EarlyconRecieverCell(UnsafeCell<Option<Reciever>>);

unsafe impl Sync for EarlyconRecieverCell {}

pub fn set_earlycon_by_cmdline() -> Result<(), &'static str> {
    let config = crate::cmdline::earlycon().ok_or("No earlycon parameter found")?;
    match config.uart_type {
        "ns16550" => {
            match config.io_type {
                "io" => {
                    #[cfg(target_arch = "x86_64")]
                    {
                        todo!()
                    }
                    #[cfg(not(target_arch = "x86_64"))]
                    {
                        return Err("io type not supported on this architecture");
                    }
                }
                _ => set_16550_mmio(&config)?,
            };
        }
        "pl011" => {
            set_pl011(&config)?;
        }
        _ => {
            return Err("unsupported earlycon uart type");
        }
    }
    unsafe {
        DEBUG_BASE = config.base_addr.unwrap_or(0);
    }
    Ok(())
}

fn set_pl011(config: &EarlyconConfig) -> Result<(), &'static str> {
    let base_addr = config
        .base_addr
        .ok_or("No base address specified for pl011 earlycon")?;
    let base_addr =
        NonNull::new(phys_to_virt(base_addr)).ok_or("Invalid base address for pl011 earlycon")?;

    let mut serial = pl011::Pl011::new(base_addr, 0);
    let tx = serial.take_tx().ok_or("no tx")?;
    let rx = serial.take_rx().ok_or("no rx")?;

    set_earlycon_sender(tx);
    set_earlycon_reciever(rx);

    Ok(())
}

fn set_16550_mmio(config: &EarlyconConfig) -> Result<(), &'static str> {
    let base_addr = config
        .base_addr
        .ok_or("No base address specified for ns16550 earlycon")?;
    let base_addr =
        NonNull::new(phys_to_virt(base_addr)).ok_or("Invalid base address for ns16550 earlycon")?;
    let width = match config.io_type {
        "mmio" => 1,
        "mmio16" => 2,
        "mmio32" => 4,
        _ => return Err("Invalid io_type for ns16550 earlycon"),
    };

    let mut serial = ns16550::Ns16550::new_mmio(base_addr, 0, width);
    let tx = serial.take_tx().ok_or("no tx")?;
    let rx = serial.take_rx().ok_or("no rx")?;

    set_earlycon_sender(tx);
    set_earlycon_reciever(rx);

    Ok(())
}
