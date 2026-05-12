use core::{cell::UnsafeCell, fmt::Write, ptr::NonNull};

use byte_unit::{Byte, UnitType};
use kernutil::memory::{MemoryDescriptor, MemoryType};
use some_serial::*;

use crate::{
    cmdline::EarlyconConfig,
    mem::{_fixmap_io, page_size},
};

pub(crate) static mut DEBUG_BASE: usize = 0;
pub(crate) static mut DEBUG_IS_MMIO: bool = false;

pub trait ArchConsoleOps {
    fn init() -> bool {
        false
    }

    fn read_byte() -> Option<u8> {
        None
    }
}

pub(crate) fn debug_to_memory_desc() -> Option<MemoryDescriptor> {
    let debug_base = unsafe { DEBUG_BASE };
    let debug_is_mmio = unsafe { DEBUG_IS_MMIO };
    if debug_base == 0 || !debug_is_mmio {
        return None;
    }

    Some(MemoryDescriptor::new_aligned(
        debug_base,
        100,
        MemoryType::Mmio,
        page_size(),
    ))
}

pub fn _print(args: core::fmt::Arguments) {
    let _ = ConFmt {}.write_fmt(args);
}

pub fn _write_bytes(bytes: &[u8]) -> usize {
    con().write_bytes(bytes)
}

pub fn _write_str(s: &str) {
    con().write_str(s);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(core::format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::console::_print(core::format_args!("{}{}", core::format_args!($($arg)*), "\n")));
}

#[macro_export]
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

pub fn print_mapping(name: &str, virt: usize, phys: usize, size: usize) {
    let fmt = Byte::from(size).get_appropriate_unit(UnitType::Binary);
    println!(
        "{:<20}: [0x{:0>16x}, 0x{:0>16x}) -> [0x{:0>16x}, 0x{:0>16x}) ({:#.2})",
        name,
        virt,
        virt + size,
        phys,
        phys + size,
        fmt
    );
}

#[allow(dead_code)]
struct ConFmt {}

impl Write for ConFmt {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let mut remaining = s;
        while let Some(pos) = remaining.find('\n') {
            // 打印 '\n' 之前的部分
            con().write_str(&remaining[..pos]);
            // 打印 "\r\n"
            con().write_str("\r\n");
            // 继续处理剩余部分
            remaining = &remaining[pos + 1..];
        }
        // 打印最后剩余的部分（如果有的话）
        if !remaining.is_empty() {
            con().write_str(remaining);
        }
        Ok(())
    }
}

fn con() -> &'static dyn Con {
    unsafe { CON }
}

pub(crate) trait Con: Send + Sync {
    fn write_bytes(&self, _bytes: &[u8]) -> usize {
        _bytes.len()
    }
    fn write_str(&self, s: &str) {
        let bytes = s.as_bytes();
        let mut buff = bytes;
        while !buff.is_empty() {
            let n = self.write_bytes(buff);
            buff = &buff[n..];
        }
    }
}

#[allow(dead_code)]
struct NoCon;
impl Con for NoCon {
    fn write_bytes(&self, _bytes: &[u8]) -> usize {
        _bytes.len()
    }
    fn write_str(&self, _s: &str) {
        // Do nothing
    }
}

static mut CON: &dyn Con = &NoCon;

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

pub fn read_byte() -> Option<u8> {
    if let Some(byte) = <crate::arch::Arch as crate::ArchTrait>::Console::read_byte() {
        return Some(byte);
    }

    unsafe {
        if let Some(ref mut reciever) = *EARLYCON_RECIEVER.0.get() {
            match reciever.read_byte() {
                Some(Ok(byte)) => Some(byte),
                _ => None,
            }
        } else {
            None
        }
    }
}

static EARLYCON_SENDER: EarlyconSenderCell = EarlyconSenderCell(UnsafeCell::new(None));

struct EarlyconSenderCell(UnsafeCell<Option<Sender>>);

unsafe impl Sync for EarlyconSenderCell {}

impl Con for EarlyconSenderCell {
    fn write_bytes(&self, bytes: &[u8]) -> usize {
        unsafe {
            if let Some(ref mut sender) = *self.0.get() {
                sender.write_bytes(bytes)
            } else {
                // No sender available, simply return the length of bytes to indicate all bytes "written"
                bytes.len()
            }
        }
    }
}

static EARLYCON_RECIEVER: EarlyconRecieverCell = EarlyconRecieverCell(UnsafeCell::new(None));

#[allow(dead_code)]
struct EarlyconRecieverCell(UnsafeCell<Option<Reciever>>);

unsafe impl Sync for EarlyconRecieverCell {}

pub fn set_earlycon_by_cmdline() -> Result<(), &'static str> {
    let config = crate::cmdline::earlycon().ok_or("No earlycon parameter found")?;
    let debug_is_mmio = match config.uart_type {
        "ns16550" => match config.io_type {
            "io" => {
                #[cfg(target_arch = "x86_64")]
                {
                    let base = config.base_addr.ok_or("missing io base address")? as u16;
                    let mut uart = some_serial::ns16550::Ns16550::new_port(base, 1_843_200);
                    let tx = uart.take_tx().ok_or("failed to take io sender")?;
                    let rx = uart.take_rx().ok_or("failed to take io receiver")?;
                    set_earlycon_sender(tx);
                    set_earlycon_reciever(rx);
                    false
                }
                #[cfg(not(target_arch = "x86_64"))]
                {
                    return Err("io type not supported on this architecture");
                }
            }
            _ => {
                set_16550_mmio(&config)?;
                true
            }
        },
        "pl011" => {
            set_pl011(&config)?;
            true
        }
        _ => {
            return Err("unsupported earlycon uart type");
        }
    };
    unsafe {
        DEBUG_BASE = config.base_addr.unwrap_or(0);
        DEBUG_IS_MMIO = debug_is_mmio;
    }
    Ok(())
}

fn set_pl011(config: &EarlyconConfig) -> Result<(), &'static str> {
    let base_addr = config
        .base_addr
        .ok_or("No base address specified for pl011 earlycon")?;
    let base_addr =
        NonNull::new(_fixmap_io(base_addr)).ok_or("Invalid base address for pl011 earlycon")?;

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
        NonNull::new(_fixmap_io(base_addr)).ok_or("Invalid base address for ns16550 earlycon")?;
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
