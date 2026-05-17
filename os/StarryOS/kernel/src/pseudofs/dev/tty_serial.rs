//! Serial TTY device (`/dev/ttySx`)
//!
//! Provides raw UART byte-pipe devices that behave like Linux serial ports.
//! All protocol logic should live in userspace; the kernel only handles
//! hardware I/O, interrupt-driven RX buffering, and termios configuration.

use alloc::collections::vec_deque::VecDeque;
use core::{any::Any, task::Context};

use ax_errno::{AxError, LinuxError};
use axfs_ng_vfs::{NodeFlags, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io};
use bytemuck::AnyBitPattern;
use dw_apb_uart::DW8250;
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PhysAddr, pa};
use crate::pseudofs::DeviceOps;
use starry_vm::{VmMutPtr, VmPtr};

// ─── UART physical addresses (RK3588) ────────────────────────────────────────
// Source: RK3588 TRM §2.1 System Memory Map + orangepi5plus.dts
//
//   UART1 → /serial@feb40000   used as /dev/ttyS1  (user data port)
//   UART3 → /serial@feb60000   used as /dev/ttyS3  (user data port)
//
// NOTE: UART2 (0xFEB50000) is the board's default debug/console UART
//       (connected to the Type-C debug port on OrangePi 5 Plus).  Do NOT
//       repurpose it here; the bootloader and early kernel console use it.
const UART1_PADDR: PhysAddr = pa!(0xfeb40000);
const UART3_PADDR: PhysAddr = pa!(0xfeb60000);

// ─── UART IRQ numbers (RK3588 GIC-600) ───────────────────────────────────────
// DTS interrupts cell format: <GIC_SPI  hwirq  IRQ_LEVEL_HIGH>
// decode_irq_cells rule: kind=0 (GIC_SPI) → kernel_irq = hwirq + 32
//
//   UART1: interrupts = <0x00 0x14C 0x04>  hwirq=0x14C(332)  → IRQ 364
//   UART3: interrupts = <0x00 0x14E 0x04>  hwirq=0x14E(334)  → IRQ 366
const UART1_IRQ: usize = 364;
const UART3_IRQ: usize = 366;

// ─── Ring-buffer capacity ────────────────────────────────────────────────────
const RX_BUF_CAP: usize = 4096;

// ─── Static per-port buffers, poll sets, and mapped vaddrs ───────────────────
// The IRQ handlers are bare fn(usize) so they can't capture context.
// We store the mapped virtual addresses in atomics, set once at init time.

use core::sync::atomic::{AtomicUsize, Ordering};

static UART1_RX_BUF: SpinNoIrq<VecDeque<u8>> = SpinNoIrq::new(VecDeque::new());
static UART3_RX_BUF: SpinNoIrq<VecDeque<u8>> = SpinNoIrq::new(VecDeque::new());
static UART1_POLL: PollSet = PollSet::new();
static UART3_POLL: PollSet = PollSet::new();
static UART1_VADDR: AtomicUsize = AtomicUsize::new(0);
static UART3_VADDR: AtomicUsize = AtomicUsize::new(0);

/// Generic IRQ handler that drains a UART FIFO into a static buffer.
/// `vaddr` is the already-mapped virtual address of the UART registers.
fn uart_irq_handler(vaddr: usize, buf: &SpinNoIrq<VecDeque<u8>>, poll: &PollSet) {
    let mut uart = DW8250::new(vaddr);
    let mut rx = buf.lock();
    let mut got_data = false;
    loop {
        if let Some(c) = uart.getchar() {
            if rx.len() < RX_BUF_CAP {
                rx.push_back(c);
            }
            got_data = true;
        } else {
            break;
        }
    }
    uart.set_ier(true);
    drop(rx);
    if got_data {
        poll.wake();
    }
}

fn uart1_irq_handler(_irq: usize) {
    uart_irq_handler(UART1_VADDR.load(Ordering::Relaxed), &UART1_RX_BUF, &UART1_POLL);
}

fn uart3_irq_handler(_irq: usize) {
    uart_irq_handler(UART3_VADDR.load(Ordering::Relaxed), &UART3_RX_BUF, &UART3_POLL);
}

// ─── Termios (raw-mode only, mirrors kernel_termios) ─────────────────────────

/// Minimal termios matching `struct termios` layout (riscv64 linux).
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct RawTermios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 19],
}

/// Minimal termios2 matching `struct termios2` layout.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct RawTermios2 {
    base: RawTermios,
    c_ispeed: u32,
    c_ospeed: u32,
}

impl RawTermios {
    /// Return a raw-mode termios (all processing disabled).
    fn raw(baud_cflag: u32) -> Self {
        // cflag: CS8 | CREAD | baud bits
        Self {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0o000060 /* CS8 */ | 0o000200 /* CREAD */ | baud_cflag,
            c_lflag: 0,
            c_line: 0,
            c_cc: [0; 19],
        }
    }
}

impl RawTermios2 {
    fn new(base: RawTermios, speed: u32) -> Self {
        Self {
            base,
            c_ispeed: speed,
            c_ospeed: speed,
        }
    }

    fn speed(&self) -> u32 {
        self.c_ospeed
    }
}

// ─── WindowSize ──────────────────────────────────────────────────────────────
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

impl Default for WinSize {
    fn default() -> Self {
        Self {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

// ─── Per-port mutable state ──────────────────────────────────────────────────

struct SerialConfig {
    termios2: RawTermios2,
    winsize: WinSize,
}

// ─── TtySerial device ────────────────────────────────────────────────────────

pub struct TtySerial {
    vaddr: usize,
    irq: usize,
    rx_buf: &'static SpinNoIrq<VecDeque<u8>>,
    poll_set: &'static PollSet,
    config: Mutex<SerialConfig>,
}

impl TtySerial {
    /// Create a new TtySerial for the given UART.
    /// `baud` is the initial baud rate (e.g. 115200 or 1500000).
    /// The caller is responsible for pin-mux configuration.
    fn new(
        paddr: PhysAddr,
        irq: usize,
        baud: u32,
        rx_buf: &'static SpinNoIrq<VecDeque<u8>>,
        poll_set: &'static PollSet,
        irq_handler: fn(usize),
        vaddr_store: &'static AtomicUsize,
    ) -> Self {
        // Map the UART registers as DEVICE memory (required for MMIO on AArch64)
        let vaddr = ax_mm::iomap(paddr, 0x1000)
            .expect("failed to iomap UART")
            .as_usize();
        vaddr_store.store(vaddr, Ordering::Relaxed);

        // Initialise hardware
        let mut uart = DW8250::new(vaddr);
        uart.init_with_baud(baud);
        uart.set_ier(true);

        // Register IRQ
        ax_hal::irq::register(irq, irq_handler);
        ax_hal::irq::set_enable(irq, true);

        let termios2 = RawTermios2::new(RawTermios::raw(0), baud);

        Self {
            vaddr,
            irq,
            rx_buf,
            poll_set,
            config: Mutex::new(SerialConfig {
                termios2,
                winsize: WinSize::default(),
            }),
        }
    }

    /// Re-configure the hardware baud rate.
    fn set_baud(&self, baud: u32) {
        let mut uart = DW8250::new(self.vaddr);
        uart.init_with_baud(baud);
        uart.set_ier(true);
        ax_hal::irq::set_enable(self.irq, true);
    }
}

// ─── DeviceOps ───────────────────────────────────────────────────────────────

impl DeviceOps for TtySerial {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        block_on(poll_io(self, IoEvents::IN, false, || {
            let mut rx = self.rx_buf.lock();
            if rx.is_empty() {
                return Err(AxError::WouldBlock);
            }
            let n = buf.len().min(rx.len());
            for i in 0..n {
                buf[i] = rx.pop_front().unwrap();
            }
            Ok(n)
        }))
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let mut uart = DW8250::new(self.vaddr);
        for &b in buf {
            uart.putchar(b);
        }
        Ok(buf.len())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        use linux_raw_sys::ioctl::*;
        match cmd {
            TCGETS => {
                let cfg = self.config.lock();
                (arg as *mut RawTermios).vm_write(cfg.termios2.base)?;
            }
            TCGETS2 => {
                let cfg = self.config.lock();
                (arg as *mut RawTermios2).vm_write(cfg.termios2)?;
            }
            TCSETS | TCSETSF | TCSETSW => {
                let new_termios: RawTermios = (arg as *const RawTermios).vm_read()?;
                let mut cfg = self.config.lock();
                let speed = cfg.termios2.speed();
                cfg.termios2 = RawTermios2::new(new_termios, speed);
                if cmd == TCSETSF {
                    self.rx_buf.lock().clear();
                }
            }
            TCSETS2 | TCSETSF2 | TCSETSW2 => {
                let new_termios2: RawTermios2 = (arg as *const RawTermios2).vm_read()?;
                let old_speed = self.config.lock().termios2.speed();
                let new_speed = new_termios2.speed();
                {
                    let mut cfg = self.config.lock();
                    cfg.termios2 = new_termios2;
                    if cmd == TCSETSF2 {
                        self.rx_buf.lock().clear();
                    }
                }
                if new_speed != 0 && new_speed != old_speed {
                    self.set_baud(new_speed);
                }
            }
            TIOCGWINSZ => {
                let cfg = self.config.lock();
                (arg as *mut WinSize).vm_write(cfg.winsize)?;
            }
            TIOCSWINSZ => {
                let ws: WinSize = (arg as *const WinSize).vm_read()?;
                self.config.lock().winsize = ws;
            }
            TCFLSH => {
                // arg: TCIFLUSH=0, TCOFLUSH=1, TCIOFLUSH=2
                if arg == 0 || arg == 2 {
                    self.rx_buf.lock().clear();
                }
                // Output flush is a no-op (we write synchronously)
            }
            // Silently accept these so that standard tcsetattr() sequences don't fail
            TCSBRK | TCSBRKP | TCXONC => {}
            _ => return Err(LinuxError::ENOTTY.into()),
        }
        Ok(0)
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

// ─── Pollable ────────────────────────────────────────────────────────────────

impl Pollable for TtySerial {
    fn poll(&self) -> IoEvents {
        let rx = self.rx_buf.lock();
        let mut events = IoEvents::OUT; // TX is always ready (synchronous)
        if !rx.is_empty() {
            events |= IoEvents::IN;
        }
        events
    }

    fn register(&self, cx: &mut Context<'_>, events: IoEvents) {
        if events.intersects(IoEvents::IN) {
            self.poll_set.register(cx.waker());
        }
    }
}

// ─── Constructor helpers used from mod.rs ────────────────────────────────────

// ─── RK3588 Pinmux reference ──────────────────────────────────────────────────
//
// All data sourced from orangepi5plus.dts (rockchip,pins = <bank pin mux pcfg>)
// and the RK3588 TRM IOMUX chapter.  mux=10 (0xA) selects UART on all pads below.
//
// UART1  0xFEB40000  IRQ 364  –  pinctrl: uart1m1-xfer  (phandle 0x169)
//   TX  GPIO1_B7  bank=1  pin_in_bank=15  mux=10
//   RX  GPIO1_B6  bank=1  pin_in_bank=14  mux=10
//   OrangePi 5 Plus 40-pin header (UART1_M1, per official wiki):
//     Pin 28  (GPIO1_B7)  → UART1_TX  → connect to peer RX
//     Pin 27  (GPIO1_B6)  → UART1_RX  → connect to peer TX
//     Pin 25  GND
//
// UART3  0xFEB60000  IRQ 366  –  pinctrl: uart3m1-xfer  (phandle 0x16b)
//   TX  GPIO3_B6  bank=3  pin_in_bank=14  mux=10
//   RX  GPIO3_B5  bank=3  pin_in_bank=13  mux=10
//   OrangePi 5 Plus 40-pin header (UART3_M1, per official wiki):
//     Pin 16  (GPIO3_B6)  → UART3_TX  → connect to peer RX
//     Pin 18  (GPIO3_B5)  → UART3_RX  → connect to peer TX
//     Pin 20  GND
//
// !! UART2 (0xFEB50000, IRQ 365) is the board debug console (Type-C port). !!
// !! It must NOT be used here.                                              !!

#[cfg(feature = "plat-dyn")]
fn apply_uart_pinmux(pins: &[(u32, u32, u32)]) {
    for &(bank, pin_in_bank, mux) in pins {
        axplat_dyn::drivers::rk3588_set_pin_mux(bank, pin_in_bank, mux);
    }
}

/// Create `/dev/ttyS1` backed by **UART1** (RK3588: `0xFEB40000`, IRQ 364).
///
/// **Pinmux** (`uart1m1-xfer`):
/// - GPIO1_B7 (PinId 47) → UART1_TX (mux 10) — 40-pin header **Pin 28**
/// - GPIO1_B6 (PinId 46) → UART1_RX (mux 10) — 40-pin header **Pin 27**
///
/// Wire your serial adapter: adapter-RX → Pin 28, adapter-TX → Pin 27, GND → Pin 25.
pub fn new_tty_s1(baud: u32) -> TtySerial {
    // Step 1: apply pinmux so GPIO1_B7/B6 are routed to UART1
    #[cfg(feature = "plat-dyn")]
    {
        info!("[ttyS1] applying pinmux: GPIO1_B7(bank=1,pin=15,mux=10)=TX  GPIO1_B6(bank=1,pin=14,mux=10)=RX");
        apply_uart_pinmux(&[
            (1, 15, 10), // GPIO1_B7 → UART1_TX  (40-pin header Pin 28)
            (1, 14, 10), // GPIO1_B6 → UART1_RX  (40-pin header Pin 27)
        ]);
        info!("[ttyS1] pinmux applied");
    }

    // Step 2: map MMIO + init DW8250 at requested baud
    info!("[ttyS1] initialising UART1 @ {:#010x}  baud={}", 0xfeb40000u64, baud);
    let dev = TtySerial::new(UART1_PADDR, UART1_IRQ, baud, &UART1_RX_BUF, &UART1_POLL, uart1_irq_handler, &UART1_VADDR);
    let vaddr = UART1_VADDR.load(Ordering::Relaxed);
    info!("[ttyS1] MMIO mapped: vaddr={:#x}  IRQ={}", vaddr, UART1_IRQ);

    // Step 3: drain any stale RX bytes before sending INIT
    {
        let mut uart = DW8250::new(vaddr);
        let mut stale = 0u32;
        while uart.getchar().is_some() { stale += 1; }
        if stale > 0 {
            warn!("[ttyS1] drained {} stale RX bytes before INIT", stale);
        }
    }

    // Step 4: send INIT frame [0xAA, 0x55, CMD_INIT=0x01, len=0x00, chk=0x01]
    dev.write_at(&[0xAA, 0x55, 0x01, 0x00, 0x01], 0).ok();
    info!("[ttyS1] INIT frame sent: [AA 55 01 00 01]");

    // Step 5: busy-poll for ACK (~500 ms window — generous for cold ESP32 boot)
    // Expected ACK: [0xAA, 0x55, 0x80, 0x01, 0x01, 0x80]
    // NOTE: IRQ handler is already active and drains UART FIFO into UART1_RX_BUF,
    //       so we must read from the shared buffer, NOT directly from the hardware.
    {
        let mut buf = [0u8; 32];
        let mut n = 0usize;
        let mut ticks = 0u32;
        const POLL_ITERS: u32 = 10_000_000;
        for _ in 0..POLL_ITERS {
            ticks += 1;
            // Drain any bytes the IRQ handler placed in the RX buffer
            {
                let mut rx = UART1_RX_BUF.lock();
                while n < buf.len() {
                    match rx.pop_front() {
                        Some(b) => { buf[n] = b; n += 1; }
                        None => break,
                    }
                }
            }
            if n >= 3 && buf[0] == 0xAA && buf[1] == 0x55 {
                break;
            }
        }

        if n == 0 {
            warn!("[ttyS1] INIT: NO RESPONSE after {} iters — possible causes:", ticks);
            warn!("[ttyS1]   1. Wrong baud rate (current: {}). ESP32 default?", baud);
            warn!("[ttyS1]   2. TX not reaching ESP32 — check Pin 28 wiring");
            warn!("[ttyS1]   3. ESP32 not powered / not running firmware");
            warn!("[ttyS1]   4. Pinmux not applied (compiled without plat-dyn?)");
        } else {
            info!("[ttyS1] INIT: received {} byte(s): {:02X?}", n, &buf[..n]);
            if n >= 3 && buf[0] == 0xAA && buf[1] == 0x55 {
                match buf[2] {
                    0x80 => info!("[ttyS1] INIT: ACK ✓  ESP32 ready"),
                    0x81 => warn!("[ttyS1] INIT: NACK ✗  ESP32 refused — wrong protocol?"),
                    c    => warn!("[ttyS1] INIT: unexpected cmd={:#04x}", c),
                }
            } else {
                warn!("[ttyS1] INIT: garbled (missing AA55 header) — baud mismatch?");
            }
        }
    }

    dev
}

/// Create `/dev/ttyS3` backed by **UART3** (RK3588: `0xFEB60000`, IRQ 366).
///
/// **Pinmux** (`uart3m1-xfer`):
/// - GPIO3_B6 (PinId 110) → UART3_TX (mux 10) — 40-pin header **Pin 16**
/// - GPIO3_B5 (PinId 109) → UART3_RX (mux 10) — 40-pin header **Pin 18**
///
/// Wire your serial adapter: adapter-RX → Pin 16, adapter-TX → Pin 18, GND → Pin 20.
pub fn new_tty_s3(baud: u32) -> TtySerial {
    #[cfg(feature = "plat-dyn")]
    apply_uart_pinmux(&[
        (3, 14, 10), // GPIO3_B6 → UART3_TX  (40-pin header Pin 16)
        (3, 13, 10), // GPIO3_B5 → UART3_RX  (40-pin header Pin 18)
    ]);

    TtySerial::new(UART3_PADDR, UART3_IRQ, baud, &UART3_RX_BUF, &UART3_POLL, uart3_irq_handler, &UART3_VADDR)
}