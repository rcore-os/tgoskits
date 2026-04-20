extern crate alloc;

use alloc::vec::Vec;
use core::{
    future::{Future, IntoFuture},
    pin::pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Duration,
};

use crab_usb::{Device, DeviceInfo, Event, EventHandler, USBHost, err::USBError};
use rdrive::DriverGeneric;

mod xhci_mmio;
mod xhci_pci;

use super::DmaImpl;

impl crab_usb::KernelOp for DmaImpl {
    fn delay(&self, duration: Duration) {
        axklib::time::busy_wait(duration);
    }
}

pub(crate) static USB_KERNEL: DmaImpl = DmaImpl;

pub struct PlatformUsbHost {
    name: &'static str,
    irq_num: Option<usize>,
    host: USBHost,
    event_handler: EventHandler,
    initialized: bool,
}

impl PlatformUsbHost {
    fn new(
        name: &'static str,
        host: USBHost,
        event_handler: EventHandler,
        irq_num: Option<usize>,
    ) -> Self {
        Self {
            name,
            irq_num,
            host,
            event_handler,
            initialized: false,
        }
    }

    pub fn init_blocking(&mut self) -> Result<(), USBError> {
        if self.initialized {
            return Ok(());
        }

        block_on_usb(self.host.init())?;
        self.initialized = true;
        Ok(())
    }

    pub fn probe_devices_blocking(&mut self) -> Result<Vec<DeviceInfo>, USBError> {
        block_on_usb(self.host.probe_devices())
    }

    pub fn open_device_blocking(&mut self, info: &DeviceInfo) -> Result<Device, USBError> {
        block_on_usb(self.host.open_device(info))
    }

    pub fn handle_irq(&self) -> Event {
        self.event_handler.handle_event()
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }
}

impl DriverGeneric for PlatformUsbHost {
    fn name(&self) -> &str {
        self.name
    }
}

pub trait PlatformDeviceUsbHost {
    fn register_usb_host(self, name: &'static str, host: USBHost, irq_num: Option<usize>);
}

impl PlatformDeviceUsbHost for rdrive::PlatformDevice {
    fn register_usb_host(self, name: &'static str, mut host: USBHost, irq_num: Option<usize>) {
        let event_handler = host.create_event_handler();
        self.register(PlatformUsbHost::new(name, host, event_handler, irq_num));
    }
}

fn pci_legacy_irq_for_address(address: rdrive::probe::pci::PciAddress) -> usize {
    const PCI_IRQ_BASE: usize = if cfg!(target_arch = "x86_64") || cfg!(target_arch = "riscv64") {
        0x20
    } else if cfg!(target_arch = "loongarch64") {
        0x10
    } else if cfg!(target_arch = "aarch64") {
        0x23
    } else {
        0
    };

    PCI_IRQ_BASE + (usize::from(address.device()) & 3)
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}

fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    match interrupt.specifier.as_slice() {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

fn block_on_usb<F: IntoFuture>(future: F) -> F::Output {
    let mut future = pin!(future.into_future());
    let waker = noop_waker();
    let mut context = Context::from_waker(&waker);

    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

fn noop_waker() -> Waker {
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_WAKER_VTABLE)) }
}

const NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |_| RawWaker::new(core::ptr::null(), &NOOP_WAKER_VTABLE),
    |_| {},
    |_| {},
    |_| {},
);
