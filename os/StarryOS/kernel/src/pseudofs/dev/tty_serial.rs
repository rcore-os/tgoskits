use alloc::collections::vec_deque::VecDeque;
use core::{
    any::Any,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::Context,
};

use ax_errno::{AxError, LinuxError};
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PhysAddr, pa};
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io};
use axfs_ng_vfs::{NodeFlags, VfsResult};
use axpoll::{IoEvents, PollSet, Pollable};
use bytemuck::AnyBitPattern;
use sg200x_bsp::{
    pinmux::Pinmux,
    soc::{FMUX_BASE, IOBLK_BASE, IOBLK_GRTC_BASE},
};
use some_serial::ns16550::dw_apb::{DwApbUart, SG2002_UART_CLOCK};
use starry_vm::{VmMutPtr, VmPtr};

use crate::pseudofs::DeviceOps;

const UART1_PADDR: PhysAddr = pa!(0x04150000);
const UART2_PADDR: PhysAddr = pa!(0x04160000);
const UART1_IRQ: usize = 45;
const UART2_IRQ: usize = 46;
const RX_BUF_CAP: usize = 4096;

/// MMIO span of a single UART block. Covers the DW APB shadow registers (USR at
/// 0x7c etc.) — one page is more than enough and keeps the mapping page-aligned.
const UART_MMIO_SIZE: usize = 0x1000;
/// MMIO span covering the pinmux register groups. FMUX (0x03001000) and the
/// Active-Domain IOBLK groups (0x03001800 + G1/G7/G10/G12 offsets) live in the
/// same 4K page; GRTC (0x05027000) is mapped separately.
const PINMUX_MMIO_SIZE: usize = 0x1000;

static UART1_RX_BUF: SpinNoIrq<VecDeque<u8>> = SpinNoIrq::new(VecDeque::new());
static UART2_RX_BUF: SpinNoIrq<VecDeque<u8>> = SpinNoIrq::new(VecDeque::new());
static UART1_POLL: PollSet = PollSet::new();
static UART2_POLL: PollSet = PollSet::new();
/// Mapped virtual base of each UART, published by `TtySerial::new` so the raw
/// IRQ handlers can reach the registers without recomputing a (now invalid on
/// dynamic platforms) `phys_to_virt` address.
static UART1_VADDR: AtomicUsize = AtomicUsize::new(0);
static UART2_VADDR: AtomicUsize = AtomicUsize::new(0);

/// Map a physical MMIO region into the kernel address space and return its
/// virtual base. Unlike `phys_to_virt`, this works on dynamic platforms where
/// `PHYS_VIRT_OFFSET == 0` and there is no static linear MMIO window — `iomap`
/// installs a real device mapping and is idempotent for already-mapped pages.
fn iomap_usize(paddr: PhysAddr, size: usize) -> usize {
    ax_mm::iomap(paddr, size)
        .unwrap_or_else(|err| panic!("failed to iomap MMIO at {paddr:#x}+{size:#x}: {err:?}"))
        .as_usize()
}

fn uart_irq_handler(vaddr: usize, buf: &SpinNoIrq<VecDeque<u8>>, poll: &PollSet) {
    let mut uart = DwApbUart::new(vaddr);
    let mut rx = buf.lock();
    let mut got_data = false;
    while let Some(c) = uart.getchar() {
        if rx.len() < RX_BUF_CAP {
            rx.push_back(c);
        }
        got_data = true;
    }
    uart.set_ier(true);
    drop(rx);
    if got_data {
        poll.wake();
    }
}

fn uart1_irq_handler(_irq: usize) {
    uart_irq_handler(
        UART1_VADDR.load(Ordering::Relaxed),
        &UART1_RX_BUF,
        &UART1_POLL,
    );
}
fn uart2_irq_handler(_irq: usize) {
    uart_irq_handler(
        UART2_VADDR.load(Ordering::Relaxed),
        &UART2_RX_BUF,
        &UART2_POLL,
    );
}

unsafe fn uart1_raw_irq_handler(
    ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    uart1_irq_handler(ctx.irq.0);
    ax_runtime::hal::irq::IrqReturn::Handled
}

unsafe fn uart2_raw_irq_handler(
    ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    uart2_irq_handler(ctx.irq.0);
    ax_runtime::hal::irq::IrqReturn::Handled
}

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

#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
struct RawTermios2 {
    base: RawTermios,
    c_ispeed: u32,
    c_ospeed: u32,
}

impl RawTermios {
    fn raw(baud_cflag: u32) -> Self {
        Self {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0o000060 | 0o000200 | baud_cflag,
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

#[repr(C)]
#[derive(Clone, Copy, Default, AnyBitPattern)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

struct SerialConfig {
    termios2: RawTermios2,
    winsize: WinSize,
}

pub struct TtySerial {
    vaddr: usize,
    irq: usize,
    rx_buf: &'static SpinNoIrq<VecDeque<u8>>,
    poll_set: &'static PollSet,
    config: Mutex<SerialConfig>,
}

impl TtySerial {
    fn new(
        paddr: PhysAddr,
        irq: usize,
        baud: u32,
        rx_buf: &'static SpinNoIrq<VecDeque<u8>>,
        poll_set: &'static PollSet,
        vaddr_slot: &AtomicUsize,
        irq_handler: ax_runtime::hal::irq::RawIrqHandler,
    ) -> Self {
        let vaddr = iomap_usize(paddr, UART_MMIO_SIZE);
        // Publish the mapped base before enabling the IRQ so the handler never
        // observes a zero (unmapped) address.
        vaddr_slot.store(vaddr, Ordering::Relaxed);
        let mut uart = DwApbUart::new(vaddr);
        uart.init_with_baud_clk(baud, SG2002_UART_CLOCK);
        uart.set_ier(true);
        let _ = ax_runtime::hal::irq::request_shared_irq(irq, irq_handler, NonNull::dangling())
            .map_err(|err| warn!("failed to request serial IRQ {irq}: {err:?}"));
        ax_runtime::hal::irq::set_enable(irq, true);
        Self {
            vaddr,
            irq,
            rx_buf,
            poll_set,
            config: Mutex::new(SerialConfig {
                termios2: RawTermios2::new(RawTermios::raw(0), baud),
                winsize: WinSize::default(),
            }),
        }
    }

    fn set_baud(&self, baud: u32) {
        let mut uart = DwApbUart::new(self.vaddr);
        uart.init_with_baud_clk(baud, SG2002_UART_CLOCK);
        uart.set_ier(true);
        ax_runtime::hal::irq::set_enable(self.irq, true);
    }
}

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
            for slot in buf.iter_mut().take(n) {
                *slot = rx.pop_front().unwrap();
            }
            Ok(n)
        }))
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let mut uart = DwApbUart::new(self.vaddr);
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
                if arg == 0 || arg == 2 {
                    self.rx_buf.lock().clear();
                }
            }
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

impl Pollable for TtySerial {
    fn poll(&self) -> IoEvents {
        let rx = self.rx_buf.lock();
        let mut events = IoEvents::OUT;
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

/// Map the pinmux register groups and build a `Pinmux` over the mapped virtual
/// bases. FMUX and the Active-Domain IOBLK groups share a page, so the two
/// `iomap` calls resolve to the same mapping (idempotent); GRTC is its own page.
fn map_pinmux() -> Pinmux {
    let fmux_vaddr = iomap_usize(pa!(FMUX_BASE), PINMUX_MMIO_SIZE);
    let ioblk_vaddr = iomap_usize(pa!(IOBLK_BASE), PINMUX_MMIO_SIZE);
    let ioblk_grtc_vaddr = iomap_usize(pa!(IOBLK_GRTC_BASE), PINMUX_MMIO_SIZE);
    unsafe { Pinmux::new(fmux_vaddr, ioblk_vaddr, ioblk_grtc_vaddr) }
}

pub fn new_tty_s1(baud: u32) -> TtySerial {
    map_pinmux().set_uart1();
    TtySerial::new(
        UART1_PADDR,
        UART1_IRQ,
        baud,
        &UART1_RX_BUF,
        &UART1_POLL,
        &UART1_VADDR,
        uart1_raw_irq_handler,
    )
}

pub fn new_tty_s2(baud: u32) -> TtySerial {
    use sg200x_bsp::pinmux::{FMUX_IIC0_SCL, FMUX_IIC0_SDA};
    let pinmux = map_pinmux();
    // Wire UART2 to IIC0_SCL/SDA (0x03001070/74), matching the
    // original StarryOS sg2002 board layout: SCL → UART2_TX,
    // SDA → UART2_RX. Wrong pinmux (e.g. pwr_gpio0/1) sends bytes
    // to floating pads and the connected device never sees them.
    pinmux.set_iic0_scl_func(FMUX_IIC0_SCL::FSEL::Value::UART2_TX);
    pinmux.set_iic0_sda_func(FMUX_IIC0_SDA::FSEL::Value::UART2_RX);
    TtySerial::new(
        UART2_PADDR,
        UART2_IRQ,
        baud,
        &UART2_RX_BUF,
        &UART2_POLL,
        &UART2_VADDR,
        uart2_raw_irq_handler,
    )
}
