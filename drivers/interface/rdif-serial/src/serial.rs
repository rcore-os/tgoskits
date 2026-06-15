use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
};

use rdif_base::DriverGeneric;
use spin::Mutex;

use super::{
    BIrqHandler, BRxQueue, BSerial, BTxQueue, InterfaceRaw, InterruptMask, SetBackError,
    TIrqHandler, TRxQueue, TTxQueue, TransBytesError,
};

pub struct SerialDyn<T: InterfaceRaw> {
    name: String,
    base_addr: usize,
    control: Arc<Mutex<T>>,
    tx_taken: Arc<AtomicBool>,
    rx_taken: Arc<AtomicBool>,
    irq_taken: Arc<AtomicBool>,
}

impl<T: InterfaceRaw> SerialDyn<T> {
    pub fn new_boxed(control: T) -> BSerial {
        let name = String::from(control.name());
        let base_addr = control.base_addr();
        Box::new(Self {
            name,
            base_addr,
            control: Arc::new(Mutex::new(control)),
            tx_taken: Arc::new(AtomicBool::new(false)),
            rx_taken: Arc::new(AtomicBool::new(false)),
            irq_taken: Arc::new(AtomicBool::new(false)),
        })
    }
}

impl<T: InterfaceRaw> super::Interface for SerialDyn<T> {
    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn set_config(&mut self, config: &crate::Config) -> Result<(), crate::ConfigError> {
        self.control.lock().set_config(config)
    }

    fn baudrate(&self) -> u32 {
        self.control.lock().baudrate()
    }

    fn data_bits(&self) -> crate::DataBits {
        self.control.lock().data_bits()
    }

    fn stop_bits(&self) -> crate::StopBits {
        self.control.lock().stop_bits()
    }

    fn parity(&self) -> crate::Parity {
        self.control.lock().parity()
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        self.control.lock().clock_freq()
    }

    fn open(&mut self) {
        self.control.lock().open();
    }

    fn close(&mut self) {
        self.control.lock().close();
    }

    fn enable_loopback(&mut self) {
        self.control.lock().enable_loopback()
    }

    fn disable_loopback(&mut self) {
        self.control.lock().disable_loopback()
    }

    fn is_loopback_enabled(&self) -> bool {
        self.control.lock().is_loopback_enabled()
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.control.lock().set_irq_mask(mask);
    }

    fn get_irq_mask(&self) -> InterruptMask {
        self.control.lock().get_irq_mask()
    }

    fn take_tx(&mut self) -> Option<BTxQueue> {
        take_flag(&self.tx_taken)?;
        Some(Box::new(TxQueue {
            base_addr: self.base_addr,
            control: self.control.clone(),
            taken: self.tx_taken.clone(),
        }))
    }

    fn take_rx(&mut self) -> Option<BRxQueue> {
        take_flag(&self.rx_taken)?;
        Some(Box::new(RxQueue {
            base_addr: self.base_addr,
            control: self.control.clone(),
            taken: self.rx_taken.clone(),
        }))
    }

    fn take_irq_handler(&mut self) -> Option<BIrqHandler> {
        take_flag(&self.irq_taken)?;
        Some(Box::new(IrqHandler {
            base_addr: self.base_addr,
            control: self.control.clone(),
            taken: self.irq_taken.clone(),
        }))
    }

    fn set_tx(&mut self, tx: BTxQueue) -> Result<(), SetBackError> {
        ensure_same_base(self.base_addr(), tx.base_addr())?;
        self.tx_taken.store(false, Ordering::Release);
        Ok(())
    }

    fn set_rx(&mut self, rx: BRxQueue) -> Result<(), SetBackError> {
        ensure_same_base(self.base_addr(), rx.base_addr())?;
        self.rx_taken.store(false, Ordering::Release);
        Ok(())
    }

    fn set_irq_handler(&mut self, irq: BIrqHandler) -> Result<(), SetBackError> {
        ensure_same_base(self.base_addr(), irq.base_addr())?;
        self.irq_taken.store(false, Ordering::Release);
        Ok(())
    }
}

impl<T: InterfaceRaw> DriverGeneric for SerialDyn<T> {
    fn name(&self) -> &str {
        &self.name
    }
}

pub struct TxQueue<T: InterfaceRaw> {
    base_addr: usize,
    control: Arc<Mutex<T>>,
    taken: Arc<AtomicBool>,
}

impl<T: InterfaceRaw> Drop for TxQueue<T> {
    fn drop(&mut self) {
        self.taken.store(false, Ordering::Release);
    }
}

impl<T: InterfaceRaw> TTxQueue for TxQueue<T> {
    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn poll(&mut self) -> crate::SerialEvent {
        self.control.lock().poll() & crate::SerialEvent::TX_READY
    }

    fn try_write(&mut self, bytes: &[u8]) -> usize {
        self.control.lock().try_write(bytes)
    }
}

pub struct RxQueue<T: InterfaceRaw> {
    base_addr: usize,
    control: Arc<Mutex<T>>,
    taken: Arc<AtomicBool>,
}

impl<T: InterfaceRaw> Drop for RxQueue<T> {
    fn drop(&mut self) {
        self.taken.store(false, Ordering::Release);
    }
}

impl<T: InterfaceRaw> TRxQueue for RxQueue<T> {
    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn poll(&mut self) -> crate::SerialEvent {
        self.control.lock().poll()
            & (crate::SerialEvent::RX_READY
                | crate::SerialEvent::RX_ERROR
                | crate::SerialEvent::OVERRUN)
    }

    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        self.control.lock().try_read(bytes)
    }
}

pub struct IrqHandler<T: InterfaceRaw> {
    base_addr: usize,
    control: Arc<Mutex<T>>,
    taken: Arc<AtomicBool>,
}

impl<T: InterfaceRaw> Drop for IrqHandler<T> {
    fn drop(&mut self) {
        self.taken.store(false, Ordering::Release);
    }
}

impl<T: InterfaceRaw> TIrqHandler for IrqHandler<T> {
    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn handle_irq(&self) -> crate::SerialEvent {
        if let Some(mut control) = self.control.try_lock() {
            control.handle_irq()
        } else {
            crate::SerialEvent::empty()
        }
    }
}

fn take_flag(flag: &AtomicBool) -> Option<()> {
    flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .ok()
        .map(drop)
}

fn ensure_same_base(want: usize, actual: usize) -> Result<(), SetBackError> {
    if want == actual {
        Ok(())
    } else {
        Err(SetBackError::new(want, actual))
    }
}
