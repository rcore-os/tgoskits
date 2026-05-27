use alloc::collections::vec_deque::VecDeque;
use core::{any::Any, task::Context};

use ax_errno::{AxError, LinuxError};
use ax_kspin::SpinNoIrq;
use ax_memory_addr::{PhysAddr, pa};
use ax_runtime::hal::mem::phys_to_virt;
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

static UART1_RX_BUF: SpinNoIrq<VecDeque<u8>> = SpinNoIrq::new(VecDeque::new());
static UART2_RX_BUF: SpinNoIrq<VecDeque<u8>> = SpinNoIrq::new(VecDeque::new());
static UART1_POLL: PollSet = PollSet::new();
static UART2_POLL: PollSet = PollSet::new();

fn uart_irq_handler(paddr: PhysAddr, buf: &SpinNoIrq<VecDeque<u8>>, poll: &PollSet) {
    let mut uart = DwApbUart::new(phys_to_virt(paddr).as_usize());
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
    uart_irq_handler(UART1_PADDR, &UART1_RX_BUF, &UART1_POLL);
}
fn uart2_irq_handler(_irq: usize) {
    uart_irq_handler(UART2_PADDR, &UART2_RX_BUF, &UART2_POLL);
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
    paddr: PhysAddr,
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
        irq_handler: fn(usize),
    ) -> Self {
        let vaddr = phys_to_virt(paddr).as_usize();
        let mut uart = DwApbUart::new(vaddr);
        uart.init_with_baud_clk(baud, SG2002_UART_CLOCK);
        uart.set_ier(true);
        ax_runtime::hal::irq::register(irq, irq_handler);
        ax_runtime::hal::irq::set_enable(irq, true);
        Self {
            paddr,
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
        let vaddr = phys_to_virt(self.paddr).as_usize();
        let mut uart = DwApbUart::new(vaddr);
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
        let vaddr = phys_to_virt(self.paddr).as_usize();
        let mut uart = DwApbUart::new(vaddr);
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

pub fn new_tty_s1(baud: u32) -> TtySerial {
    let pinmux = unsafe {
        Pinmux::new(
            FMUX_BASE + ax_config::plat::PHYS_VIRT_OFFSET,
            IOBLK_BASE + ax_config::plat::PHYS_VIRT_OFFSET,
            IOBLK_GRTC_BASE + ax_config::plat::PHYS_VIRT_OFFSET,
        )
    };
    pinmux.set_uart1();
    TtySerial::new(
        UART1_PADDR,
        UART1_IRQ,
        baud,
        &UART1_RX_BUF,
        &UART1_POLL,
        uart1_irq_handler,
    )
}

pub fn new_tty_s2(baud: u32) -> TtySerial {
    use sg200x_bsp::pinmux::{FMUX_IIC0_SCL, FMUX_IIC0_SDA};
    let pinmux = unsafe {
        Pinmux::new(
            FMUX_BASE + ax_config::plat::PHYS_VIRT_OFFSET,
            IOBLK_BASE + ax_config::plat::PHYS_VIRT_OFFSET,
            IOBLK_GRTC_BASE + ax_config::plat::PHYS_VIRT_OFFSET,
        )
    };
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
        uart2_irq_handler,
    )
}
