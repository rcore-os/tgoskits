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
    ConfigError, PollingUart, SerialEvent, TransferError,
    ns16550::{self, Mmio, Ns16550},
    pl011,
};

use crate::{
    cmdline::EarlyconConfig,
    mem::{_fixmap_io, page_size},
};

mod handoff;
pub use handoff::ConsoleHandoffError;

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
    let Some(_access) = handoff::try_enter_early() else {
        return;
    };
    let _ = ConFmt {}.write_fmt(args);
}

pub fn _write_bytes(bytes: &[u8]) -> usize {
    let Some(_access) = handoff::try_enter_early() else {
        return bytes.len();
    };
    con().write_bytes(bytes)
}

pub fn _write_str(s: &str) {
    let Some(_access) = handoff::try_enter_early() else {
        return;
    };
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

/// Enters `Preparing`, blocks new early accesses, and drains in-flight access.
pub fn begin_runtime_handoff() -> Result<(), ConsoleHandoffError> {
    handoff::begin()
}

/// Publishes runtime ownership after runtime configuration and routing succeed.
pub fn commit_runtime_handoff() -> Result<(), ConsoleHandoffError> {
    handoff::commit()
}

/// Restores early ownership after a recoverable handoff failure.
pub fn rollback_runtime_handoff() -> Result<(), ConsoleHandoffError> {
    handoff::rollback()
}

/// Fails closed when the hardware ownership state cannot be proven safe.
pub fn fail_runtime_handoff_closed() {
    handoff::fail_closed();
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
    let Some(_access) = handoff::try_enter_early() else {
        return;
    };
    EARLYCON.set_serial(serial);
    unsafe { set_out(&EARLYCON) };
}

/// Failure when binding the early console to a PL011 register block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Pl011BindError {
    /// The register window of the device could not be resolved.
    InvalidAddress,
    /// The device stayed busy until the driver's polling budget expired.
    Busy,
    /// The device rejected the early console configuration.
    Config,
}

impl From<ConfigError> for Pl011BindError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Timeout => Self::Busy,
            ConfigError::InvalidBaudrate
            | ConfigError::UnsupportedDataBits
            | ConfigError::UnsupportedStopBits
            | ConfigError::UnsupportedParity
            | ConfigError::RegisterError => Self::Config,
        }
    }
}

impl Pl011BindError {
    /// Message describing the failure on the `earlycon=` command line path.
    fn message(self) -> &'static str {
        match self {
            Self::InvalidAddress => "Invalid base address for pl011 earlycon",
            Self::Busy => "PL011 earlycon remained busy until the polling timeout",
            Self::Config => "Failed to initialize PL011 earlycon",
        }
    }
}

/// Binds the early console to the PL011 register block of physical device `paddr`.
///
/// The first successful binding of a physical device initializes it and
/// propagates failure. Selecting the same device again after an MMU or
/// relocation change only moves the software register window: the UART keeps
/// running, so pending TX bytes are neither drained nor discarded.
pub(crate) fn bind_earlycon_pl011(paddr: usize, clock: u32) -> Result<(), Pl011BindError> {
    let registers = NonNull::new(_fixmap_io(paddr)).ok_or(Pl011BindError::InvalidAddress)?;
    let Some(_access) = handoff::try_enter_early() else {
        // A console that handed ownership to the runtime owner ignores new
        // bindings, matching `set_earlycon_serial`.
        return Ok(());
    };
    EARLYCON.bind_pl011(paddr, registers, clock)?;
    // SAFETY: `EARLYCON` has a `'static` lifetime and owns the serial that backs
    // this early console, so the published `Con` reference outlives it.
    unsafe { set_out(&EARLYCON) };
    Ok(())
}

pub fn read_byte() -> Option<u8> {
    let _access = handoff::try_enter_early()?;
    if let Some(byte) = <crate::arch::Arch as crate::ArchTrait>::Console::read_byte() {
        return Some(byte);
    }

    EARLYCON.read_byte()
}

pub fn irq_num() -> Option<usize> {
    let _access = handoff::try_enter_early()?;
    <crate::arch::Arch as crate::ArchTrait>::Console::irq_num()
}

pub fn set_input_irq_enabled(enabled: bool) {
    let Some(_access) = handoff::try_enter_early() else {
        return;
    };
    <crate::arch::Arch as crate::ArchTrait>::Console::set_input_irq_enabled(enabled);
}

pub fn handle_irq() -> u32 {
    let Some(_access) = handoff::try_enter_early() else {
        return 0;
    };
    <crate::arch::Arch as crate::ArchTrait>::Console::handle_irq()
}

static EARLYCON: EarlyconCell = EarlyconCell(EarlyconMutex::new(EarlyconState::new()));

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
        // Do not replace this with an allocating mutex or the rdif runtime wrapper.
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

        let irq_enabled = ax_cpu::interrupt::irqs_enabled();
        ax_cpu::interrupt::disable_irqs();
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
        if irq_enabled {
            ax_cpu::interrupt::enable_irqs();
        }
        ret
    }
}

/// Early console state owned by this boot owner.
struct EarlyconState {
    serial: Option<EarlySerial>,
    /// Physical base of the PL011 whose hardware initialization succeeded.
    ///
    /// Only this device may be rebound without initializing it again. A device
    /// whose initialization failed has no entry here, so a later selection of
    /// the same device still initializes the UART.
    initialized_pl011_paddr: Option<usize>,
}

impl EarlyconState {
    const fn new() -> Self {
        Self {
            serial: None,
            initialized_pl011_paddr: None,
        }
    }

    /// Binds the early console to the PL011 register block at `registers`.
    ///
    /// `paddr` identifies the physical device, while `registers` is the
    /// software window that is valid for the current address translation. The
    /// first binding of a device initializes the hardware; binding the same
    /// device again only republishes the window, because `open()` on a running
    /// UART waits for TX to drain, disables the UART and clears its FIFO, which
    /// discards output the host has not consumed yet.
    fn bind_pl011(
        &mut self,
        paddr: usize,
        registers: NonNull<u8>,
        clock: u32,
    ) -> Result<(), Pl011BindError> {
        let mut serial = pl011::Pl011::new(registers, clock);
        if self.initialized_pl011_paddr != Some(paddr) {
            serial.open()?;
            self.initialized_pl011_paddr = Some(paddr);
        }
        self.serial = Some(EarlySerial::new(EarlySerialRaw::Pl011(serial)));
        Ok(())
    }
}

struct EarlyconCell(EarlyconMutex<EarlyconState>);

impl EarlyconCell {
    fn set_serial(&self, serial: EarlySerial) {
        self.0.with_lock(|earlycon| earlycon.serial = Some(serial));
    }

    /// Initializes or rebinds the PL011 and publishes the result.
    ///
    /// The identity check and the installation share one critical section, so
    /// no other console user observes a half-bound console and the bounded
    /// `open()` wait only runs for a device that is not bound yet.
    fn bind_pl011(
        &self,
        paddr: usize,
        registers: NonNull<u8>,
        clock: u32,
    ) -> Result<(), Pl011BindError> {
        self.0
            .with_lock(|earlycon| earlycon.bind_pl011(paddr, registers, clock))
    }

    fn read_byte(&self) -> Option<u8> {
        self.0.with_lock(|earlycon| {
            let serial = earlycon.serial.as_mut()?;

            let mut byte = [0];
            match serial.try_read(&mut byte) {
                Ok(1) => Some(byte[0]),
                Err(err) if err.bytes_transferred == 1 => Some(byte[0]),
                _ => None,
            }
        })
    }

    fn try_write(&self, bytes: &[u8]) -> Option<usize> {
        self.0.with_lock(|earlycon| {
            earlycon
                .serial
                .as_mut()
                .map(|serial| serial.try_write(bytes))
        })
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
    extern crate std;

    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::{sync::Mutex, thread};

    use super::*;

    struct CountingCon;

    static WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    impl Con for CountingCon {
        fn write_bytes(&self, bytes: &[u8]) -> usize {
            WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
            bytes.len()
        }
    }

    static COUNTING_CON: CountingCon = CountingCon;

    struct BlockingCon;

    static BLOCKING_ENTERED: AtomicBool = AtomicBool::new(false);
    static BLOCKING_RELEASED: AtomicBool = AtomicBool::new(false);
    static BEGIN_FINISHED: AtomicBool = AtomicBool::new(false);

    impl Con for BlockingCon {
        fn write_bytes(&self, bytes: &[u8]) -> usize {
            BLOCKING_ENTERED.store(true, Ordering::Release);
            while !BLOCKING_RELEASED.load(Ordering::Acquire) {
                thread::yield_now();
            }
            bytes.len()
        }
    }

    static BLOCKING_CON: BlockingCon = BlockingCon;

    fn reset_handoff() {
        handoff::reset();
        WRITE_CALLS.store(0, Ordering::Relaxed);
        BLOCKING_ENTERED.store(false, Ordering::Release);
        BLOCKING_RELEASED.store(false, Ordering::Release);
        BEGIN_FINISHED.store(false, Ordering::Release);
    }

    #[test]
    fn successful_handoff_consumes_without_touching_boot_console() {
        let _test = TEST_LOCK.lock().unwrap();
        reset_handoff();

        unsafe { set_out(&COUNTING_CON) };

        assert_eq!(_write_bytes(b"before"), 6);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 1);

        begin_runtime_handoff().unwrap();
        assert_eq!(handoff::state(), handoff::PREPARING);
        commit_runtime_handoff().unwrap();
        assert_eq!(handoff::state(), handoff::RUNTIME);

        assert_eq!(_write_bytes(b"after"), 5);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 1);

        reset_handoff();
    }

    #[test]
    fn preparing_waits_for_in_flight_write_and_blocks_new_access() {
        let _test = TEST_LOCK.lock().unwrap();
        reset_handoff();
        unsafe { set_out(&BLOCKING_CON) };

        let writer = thread::spawn(|| assert_eq!(_write_bytes(b"in-flight"), 9));
        while !BLOCKING_ENTERED.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let handoff = thread::spawn(|| {
            begin_runtime_handoff().unwrap();
            BEGIN_FINISHED.store(true, Ordering::Release);
        });
        while handoff::state() != handoff::PREPARING {
            thread::yield_now();
        }

        assert!(!BEGIN_FINISHED.load(Ordering::Acquire));
        assert_eq!(_write_bytes(b"blocked"), 7);
        BLOCKING_RELEASED.store(true, Ordering::Release);
        writer.join().unwrap();
        handoff.join().unwrap();
        assert!(BEGIN_FINISHED.load(Ordering::Acquire));

        rollback_runtime_handoff().unwrap();
        reset_handoff();
    }

    #[test]
    fn recoverable_failure_rolls_back_to_early_console() {
        let _test = TEST_LOCK.lock().unwrap();
        reset_handoff();
        unsafe { set_out(&COUNTING_CON) };

        begin_runtime_handoff().unwrap();
        assert_eq!(_write_bytes(b"preparing"), 9);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 0);
        rollback_runtime_handoff().unwrap();

        assert_eq!(handoff::state(), handoff::EARLY);
        assert_eq!(_write_bytes(b"early"), 5);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 1);
        reset_handoff();
    }

    #[test]
    fn uncertain_failure_closes_early_tx_rx_and_irq_access() {
        let _test = TEST_LOCK.lock().unwrap();
        reset_handoff();
        unsafe { set_out(&COUNTING_CON) };

        begin_runtime_handoff().unwrap();
        fail_runtime_handoff_closed();

        assert_eq!(handoff::state(), handoff::FAILED_CLOSED);
        assert_eq!(_write_bytes(b"closed"), 6);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 0);
        assert_eq!(read_byte(), None);
        assert_eq!(irq_num(), None);
        assert_eq!(handle_irq(), 0);
        set_input_irq_enabled(true);
        reset_handoff();
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
    bind_earlycon_pl011(base_addr, 0).map_err(Pl011BindError::message)
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

#[cfg(test)]
mod pl011_binding_tests {
    extern crate std;

    use core::{cell::UnsafeCell, ptr};
    use std::boxed::Box;

    use super::*;

    // The fixture is programmed through the same volatile register accesses as
    // the production driver, so these tests observe real PL011 driver behaviour
    // instead of a mock. MMIO is replaced by ordinary memory here; the MMU and
    // page-table path that produces the window is covered by the boot runtime
    // tests, not by this rule.
    const WINDOW_SIZE: usize = 0x1000;
    const WINDOW_WORDS: usize = WINDOW_SIZE / 4;

    const DR: usize = 0x00;
    const FR: usize = 0x18;
    const IBRD: usize = 0x24;
    const FBRD: usize = 0x28;
    const LCR_H: usize = 0x2c;
    const CR: usize = 0x30;
    const IMSC: usize = 0x38;

    const FR_CTS: u32 = 1 << 0;
    const FR_BUSY: u32 = 1 << 3;
    const FR_RXFE: u32 = 1 << 4;
    const FR_TXFF: u32 = 1 << 5;
    const FR_TXFE: u32 = 1 << 7;

    const CR_UARTEN: u32 = 1 << 0;
    const CR_TXE: u32 = 1 << 8;
    const CR_RXE: u32 = 1 << 9;
    const CR_TRANSCEIVE: u32 = CR_UARTEN | CR_TXE | CR_RXE;

    /// Eight-bit words with FIFOs still disabled.
    const LCR_H_EIGHT_BIT: u32 = 0x60;
    const LCR_H_FEN: u32 = 1 << 4;

    const PL011_PADDR: usize = 0x0900_0000;
    const OTHER_PL011_PADDR: usize = 0x0900_1000;
    const RETRY_PL011_PADDR: usize = 0x0900_2000;
    const CLOCK: u32 = 24_000_000;

    #[repr(align(4096))]
    struct Pl011Window {
        registers: UnsafeCell<[u8; WINDOW_SIZE]>,
    }

    impl Pl011Window {
        fn new() -> Box<Self> {
            Box::new(Self {
                registers: UnsafeCell::new([0; WINDOW_SIZE]),
            })
        }

        fn registers(&self) -> NonNull<u8> {
            NonNull::new(self.registers.get().cast::<u8>())
                .expect("the fixture window is always addressable")
        }

        fn write(&self, offset: usize, value: u32) {
            // SAFETY: `offset` is one of the 32-bit register offsets declared in
            // this module and the fixture owns `WINDOW_SIZE` bytes, so the access
            // stays inside the fixture. Volatile accesses keep the driver's
            // register writes and these observations in program order.
            unsafe {
                ptr::write_volatile(self.registers.get().cast::<u8>().add(offset).cast(), value)
            }
        }

        fn read(&self, offset: usize) -> u32 {
            // SAFETY: see `write`.
            unsafe { ptr::read_volatile(self.registers.get().cast::<u8>().add(offset).cast()) }
        }

        /// Returns the whole register block so any register write becomes visible.
        fn image(&self) -> [u32; WINDOW_WORDS] {
            let mut image = [0; WINDOW_WORDS];
            for (index, word) in image.iter_mut().enumerate() {
                *word = self.read(index * 4);
            }
            image
        }

        fn assert_image_unchanged(&self, before: &[u32; WINDOW_WORDS]) {
            for (index, expected) in before.iter().enumerate() {
                assert_eq!(
                    self.read(index * 4),
                    *expected,
                    "register at {:#x} changed",
                    index * 4
                );
            }
        }
    }

    #[test]
    fn rebinding_initialized_pl011_keeps_the_running_device() {
        let mut state = EarlyconState::new();
        let device = Pl011Window::new();
        device.write(IBRD, 13);
        device.write(FBRD, 2);
        device.write(LCR_H, LCR_H_EIGHT_BIT);
        device.write(FR, FR_TXFE | FR_RXFE);

        state
            .bind_pl011(PL011_PADDR, device.registers(), CLOCK)
            .expect("the first binding initializes the PL011");
        assert_eq!(device.read(CR), CR_TRANSCEIVE);
        assert_eq!(device.read(LCR_H), LCR_H_EIGHT_BIT | LCR_H_FEN);
        assert_eq!(device.read(IMSC), 0);
        assert_eq!(device.read(IBRD), 13);
        assert_eq!(device.read(FBRD), 2);
        let initialized = device.image();

        // After the MMU is enabled the same device is reached through another
        // software window while the host has not drained the FIFO yet. This is
        // the BUSY/TXFF/RXFE/CTS state recorded for the #2672 stall.
        let remapped = Pl011Window::new();
        remapped.write(IBRD, 13);
        remapped.write(FBRD, 2);
        remapped.write(LCR_H, LCR_H_EIGHT_BIT | LCR_H_FEN);
        remapped.write(CR, CR_TRANSCEIVE);
        remapped.write(FR, FR_BUSY | FR_TXFF | FR_RXFE | FR_CTS);
        let running = remapped.image();

        state
            .bind_pl011(PL011_PADDR, remapped.registers(), CLOCK)
            .expect("rebinding a running PL011 must not wait for TX to drain");
        remapped.assert_image_unchanged(&running);

        // The rebound console keeps working through the new window, and the
        // device left behind is not touched again.
        remapped.write(FR, FR_TXFE | FR_RXFE);
        let serial = state
            .serial
            .as_mut()
            .expect("the rebind publishes the early serial");
        assert_eq!(serial.try_write(b"ok"), 2);
        assert_eq!(remapped.read(DR), u32::from(b'k'));
        device.assert_image_unchanged(&initialized);
    }

    #[test]
    fn unbound_pl011_still_requires_initialization() {
        let mut state = EarlyconState::new();
        let first = Pl011Window::new();
        first.write(FR, FR_TXFE | FR_RXFE);
        state
            .bind_pl011(PL011_PADDR, first.registers(), CLOCK)
            .expect("the first binding initializes the PL011");

        // A different physical PL011 is a different device, so the successful
        // binding of another UART cannot authorize skipping its initialization.
        let second = Pl011Window::new();
        second.write(FR, FR_TXFE | FR_RXFE);
        state
            .bind_pl011(OTHER_PL011_PADDR, second.registers(), CLOCK)
            .expect("a newly selected PL011 is initialized");
        assert_eq!(second.read(CR), CR_TRANSCEIVE);

        // A device that cannot become idle reports the bounded wait failure,
        // without writing any register and without being recorded as initialized.
        let stuck = Pl011Window::new();
        stuck.write(FR, FR_BUSY);
        let untouched = stuck.image();
        assert_eq!(
            state.bind_pl011(RETRY_PL011_PADDR, stuck.registers(), CLOCK),
            Err(Pl011BindError::Busy)
        );
        stuck.assert_image_unchanged(&untouched);

        let serial = state
            .serial
            .as_mut()
            .expect("the failed binding keeps the previous console");
        assert_eq!(serial.try_write(b"y"), 1);
        assert_eq!(second.read(DR), u32::from(b'y'));

        // The failed device was not remembered, so selecting it again initializes it.
        stuck.write(FR, FR_TXFE | FR_RXFE);
        state
            .bind_pl011(RETRY_PL011_PADDR, stuck.registers(), CLOCK)
            .expect("a device that failed once is initialized on the next binding");
        assert_eq!(stuck.read(CR), CR_TRANSCEIVE);
    }
}
