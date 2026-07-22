use core::{
    cell::UnsafeCell,
    fmt::Write,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use byte_unit::{Byte, UnitType};
use kernutil::memory::{MemoryDescriptor, MemoryType};
#[cfg(target_arch = "x86_64")]
use some_serial::ns16550::Port;
use some_serial::{
    PollingUart, SerialEvent, TransferError,
    ns16550::{self, Mmio, Ns16550},
    pl011,
};

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

    fn irq_num() -> Option<usize> {
        None
    }

    fn set_input_irq_enabled(_enabled: bool) {}

    fn handle_irq() -> u32 {
        0
    }
}

pub const CONSOLE_IRQ_RX_READY: u32 = 1 << 0;
pub const CONSOLE_IRQ_RX_ERROR: u32 = 1 << 1;
pub const CONSOLE_IRQ_OVERRUN: u32 = 1 << 2;

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
    if runtime_output_claimed() {
        return;
    }
    let _ = ConFmt {}.write_fmt(args);
}

pub fn _write_bytes(bytes: &[u8]) -> usize {
    if runtime_output_claimed() {
        return bytes.len();
    }
    con().write_bytes(bytes)
}

pub fn _write_str(s: &str) {
    if runtime_output_claimed() {
        return;
    }
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
static RUNTIME_OUTPUT_CLAIMED: AtomicBool = AtomicBool::new(false);

pub(crate) unsafe fn set_out(v: &'static dyn Con) {
    unsafe {
        CON = v;
    }
}

/// Marks the boot console output path as superseded by a runtime console.
///
/// Once an OS serial/tty runtime owns the UART registers, the boot console must
/// not write the same hardware directly. It still reports bytes as consumed so
/// generic logging paths cannot spin forever after the handoff.
pub fn claim_runtime_output() {
    RUNTIME_OUTPUT_CLAIMED.store(true, Ordering::Release);
}

#[cfg(not(test))]
fn runtime_output_claimed() -> bool {
    // On AArch64, exclusive atomic instructions such as LDXR/LDAXR are not
    // reliable before the MMU is enabled. Keep the pre-MMU boot console path
    // free of atomic reads and only honor the runtime handoff afterwards.
    crate::mem::mmu::is_mmu_enabled() && RUNTIME_OUTPUT_CLAIMED.load(Ordering::Acquire)
}

#[cfg(test)]
fn runtime_output_claimed() -> bool {
    RUNTIME_OUTPUT_CLAIMED.load(Ordering::Acquire)
}

pub struct EarlySerial {
    raw: EarlySerialRaw,
    tx_state: SerialEvent,
    rx_state: SerialEvent,
}

pub enum EarlySerialRaw {
    Ns16550Mmio(Ns16550<Mmio>),
    #[cfg(target_arch = "x86_64")]
    Ns16550Port(Ns16550<Port>),
    Pl011(pl011::Pl011),
}

impl EarlySerial {
    pub fn new(raw: EarlySerialRaw) -> Self {
        Self {
            raw,
            tx_state: SerialEvent::empty(),
            rx_state: SerialEvent::empty(),
        }
    }

    pub fn try_write(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        while written < bytes.len() {
            self.refresh_status();
            if !self.tx_state.tx_ready() {
                break;
            }
            self.with_raw(|serial| serial.write_byte(bytes[written]));
            self.tx_state
                .remove(SerialEvent::TX_READY | SerialEvent::TX_ERROR);
            written += 1;
        }
        written
    }

    pub fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, some_serial::TransBytesError> {
        let mut read = 0;
        let mut first_error = None;
        for byte in bytes.iter_mut() {
            self.refresh_status();
            if !self.rx_state.rx_ready() && !self.rx_state.rx_error() {
                break;
            }
            let status = self.rx_state;
            self.rx_state
                .remove(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
            match self.with_raw(|serial| serial.read_byte(status)) {
                Some(Ok(b)) => {
                    *byte = b;
                    read += 1;
                }
                Some(Err(TransferError::Overrun(b))) => {
                    *byte = b;
                    read += 1;
                    first_error.get_or_insert(TransferError::Overrun(b));
                }
                Some(Err(err)) => {
                    first_error.get_or_insert(err);
                }
                None => break,
            }
        }
        if let Some(kind) = first_error {
            Err(some_serial::TransBytesError {
                bytes_transferred: read,
                kind,
            })
        } else {
            Ok(read)
        }
    }

    fn refresh_status(&mut self) {
        let event = self.with_raw(|serial| serial.poll_status());
        self.tx_state |= event & (SerialEvent::TX_READY | SerialEvent::TX_ERROR);
        self.rx_state |=
            event & (SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
    }

    fn with_raw<R>(&mut self, f: impl FnOnce(&mut dyn PollingUart) -> R) -> R {
        match &mut self.raw {
            EarlySerialRaw::Ns16550Mmio(serial) => f(serial),
            #[cfg(target_arch = "x86_64")]
            EarlySerialRaw::Ns16550Port(serial) => f(serial),
            EarlySerialRaw::Pl011(serial) => f(serial),
        }
    }
}

pub fn set_earlycon_serial(serial: EarlySerial) {
    EARLYCON.set_serial(serial);
    unsafe { set_out(&EARLYCON) };
}

pub fn read_byte() -> Option<u8> {
    if let Some(byte) = <crate::arch::Arch as crate::ArchTrait>::Console::read_byte() {
        return Some(byte);
    }

    EARLYCON.read_byte()
}

pub fn irq_num() -> Option<usize> {
    <crate::arch::Arch as crate::ArchTrait>::Console::irq_num()
}

pub fn set_input_irq_enabled(enabled: bool) {
    <crate::arch::Arch as crate::ArchTrait>::Console::set_input_irq_enabled(enabled);
}

pub fn handle_irq() -> u32 {
    <crate::arch::Arch as crate::ArchTrait>::Console::handle_irq()
}

static EARLYCON: EarlyconCell = EarlyconCell(EarlyconMutex::new(None));

struct EarlyconMutex<T> {
    locked: AtomicBool,
    inner: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for EarlyconMutex<T> {}

impl<T> EarlyconMutex<T> {
    const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            inner: UnsafeCell::new(value),
        }
    }

    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        // Do not replace this with spin::Mutex or the rdif runtime wrapper.
        // someboot runs before the normal allocator is available, so early
        // serial cannot allocate Box/Arc-backed runtime state and must keep a
        // raw register-level enum here. On AArch64, exclusive atomic
        // instructions such as LDXR/LDAXR are not reliable before the MMU is
        // enabled, so the early console must also avoid touching the atomic
        // lock word on that path. Before MMU setup, someboot is still in the
        // single-core early-output phase and can access the serial object
        // directly; after MMU setup, the custom atomic lock below provides real
        // exclusion for later console users.
        if !crate::mem::mmu::is_mmu_enabled() {
            return unsafe { f(&mut *self.inner.get()) };
        }

        let irq_enabled = crate::irq::irq_local_is_enabled();
        crate::irq::irq_local_set_enable(false);
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        let ret = unsafe { f(&mut *self.inner.get()) };
        self.locked.store(false, Ordering::Release);
        crate::irq::irq_local_set_enable(irq_enabled);
        ret
    }
}

struct EarlyconCell(EarlyconMutex<Option<EarlySerial>>);

impl EarlyconCell {
    fn set_serial(&self, serial: EarlySerial) {
        self.0.with_lock(|earlycon| *earlycon = Some(serial));
    }

    fn read_byte(&self) -> Option<u8> {
        self.0.with_lock(|earlycon| {
            let serial = earlycon.as_mut()?;

            let mut byte = [0];
            match serial.try_read(&mut byte) {
                Ok(1) => Some(byte[0]),
                Err(err) if err.bytes_transferred == 1 => Some(byte[0]),
                _ => None,
            }
        })
    }

    fn try_write(&self, bytes: &[u8]) -> Option<usize> {
        self.0
            .with_lock(|earlycon| earlycon.as_mut().map(|serial| serial.try_write(bytes)))
    }
}

impl Con for EarlyconCell {
    fn write_bytes(&self, bytes: &[u8]) -> usize {
        const MAX_NO_PROGRESS_SPINS: usize = 1 << 20;

        let mut written = 0;
        let mut no_progress_spins = 0;
        while written < bytes.len() {
            let Some(n) = self.try_write(&bytes[written..]) else {
                return bytes.len();
            };
            if n == 0 {
                no_progress_spins += 1;
                if no_progress_spins >= MAX_NO_PROGRESS_SPINS {
                    // Early console output is best-effort. If the UART stops
                    // accepting bytes, report the rest as consumed so boot does
                    // not hang inside logging.
                    return bytes.len();
                }
                core::hint::spin_loop();
                continue;
            }
            no_progress_spins = 0;
            written += n;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountingCon;

    static WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);

    impl Con for CountingCon {
        fn write_bytes(&self, bytes: &[u8]) -> usize {
            WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
            bytes.len()
        }
    }

    static COUNTING_CON: CountingCon = CountingCon;

    #[test]
    fn runtime_output_claim_consumes_without_touching_boot_console() {
        WRITE_CALLS.store(0, Ordering::Relaxed);
        RUNTIME_OUTPUT_CLAIMED.store(false, Ordering::Relaxed);

        unsafe { set_out(&COUNTING_CON) };

        assert_eq!(_write_bytes(b"before"), 6);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 1);

        claim_runtime_output();

        assert_eq!(_write_bytes(b"after"), 5);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 1);

        RUNTIME_OUTPUT_CLAIMED.store(false, Ordering::Relaxed);
    }
}

pub fn set_earlycon_by_cmdline() -> Result<(), &'static str> {
    let config = crate::cmdline::earlycon().ok_or("No earlycon parameter found")?;
    let debug_is_mmio = match config.uart_type {
        "ns16550" => match config.io_type {
            "io" => {
                #[cfg(target_arch = "x86_64")]
                {
                    let base = config.base_addr.ok_or("missing io base address")? as u16;
                    let mut uart = some_serial::ns16550::Ns16550::new_port(base, 1_843_200);
                    uart.open();
                    set_earlycon_serial(EarlySerial::new(EarlySerialRaw::Ns16550Port(uart)));
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
        DEBUG_BASE = config
            .base_addr
            .map(<crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr)
            .unwrap_or(0);
        DEBUG_IS_MMIO = debug_is_mmio;
    }
    Ok(())
}

fn set_pl011(config: &EarlyconConfig) -> Result<(), &'static str> {
    let base_addr = earlycon_base_addr(config, "No base address specified for pl011 earlycon")?;
    let base_addr =
        NonNull::new(_fixmap_io(base_addr)).ok_or("Invalid base address for pl011 earlycon")?;

    let mut serial = pl011::Pl011::new(base_addr, 0);
    serial.open();
    set_earlycon_serial(EarlySerial::new(EarlySerialRaw::Pl011(serial)));

    Ok(())
}

fn set_16550_mmio(config: &EarlyconConfig) -> Result<(), &'static str> {
    let base_addr = earlycon_base_addr(config, "No base address specified for ns16550 earlycon")?;
    let base_addr =
        NonNull::new(_fixmap_io(base_addr)).ok_or("Invalid base address for ns16550 earlycon")?;
    let width = match config.io_type {
        "mmio" => 1,
        "mmio16" => 2,
        "mmio32" => 4,
        _ => return Err("Invalid io_type for ns16550 earlycon"),
    };

    let mut serial = ns16550::Ns16550::new_mmio(base_addr, 0, width);
    serial.open();
    set_earlycon_serial(EarlySerial::new(EarlySerialRaw::Ns16550Mmio(serial)));

    Ok(())
}

fn earlycon_base_addr(
    config: &EarlyconConfig,
    missing: &'static str,
) -> Result<usize, &'static str> {
    let addr = config.base_addr.ok_or(missing)?;
    Ok(<crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(
        addr,
    ))
}
