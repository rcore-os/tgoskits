use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
};

use rdif_base::DriverGeneric;

use super::{
    BIrqHandler, BRxQueue, BSerial, BTxQueue, InterfaceRaw, InterruptMask, SetBackError,
    TIrqHandler, TRxQueue, TTxQueue, TransBytesError,
};

pub struct SerialDyn<T: InterfaceRaw> {
    name: String,
    control: T,
    shared: T::SharedState,
    tx_taken: Arc<AtomicBool>,
    rx_taken: Arc<AtomicBool>,
    irq_taken: Arc<AtomicBool>,
}

impl<T: InterfaceRaw> SerialDyn<T> {
    pub fn new_boxed(control: T) -> BSerial {
        let name = String::from(control.name());
        let shared = control.new_shared_state();
        Box::new(Self {
            name,
            control,
            shared,
            tx_taken: Arc::new(AtomicBool::new(false)),
            rx_taken: Arc::new(AtomicBool::new(false)),
            irq_taken: Arc::new(AtomicBool::new(false)),
        })
    }
}

impl<T: InterfaceRaw> super::Interface for SerialDyn<T> {
    fn base_addr(&self) -> usize {
        self.control.base_addr()
    }

    fn set_config(&mut self, config: &crate::Config) -> Result<(), crate::ConfigError> {
        self.control.set_config(config)
    }

    fn baudrate(&self) -> u32 {
        self.control.baudrate()
    }

    fn data_bits(&self) -> crate::DataBits {
        self.control.data_bits()
    }

    fn stop_bits(&self) -> crate::StopBits {
        self.control.stop_bits()
    }

    fn parity(&self) -> crate::Parity {
        self.control.parity()
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        self.control.clock_freq()
    }

    fn open(&mut self) {
        self.control.open();
    }

    fn close(&mut self) {
        self.control.close();
    }

    fn enable_loopback(&mut self) {
        self.control.enable_loopback()
    }

    fn disable_loopback(&mut self) {
        self.control.disable_loopback()
    }

    fn is_loopback_enabled(&self) -> bool {
        self.control.is_loopback_enabled()
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.control.set_irq_mask(mask);
    }

    fn get_irq_mask(&self) -> InterruptMask {
        self.control.get_irq_mask()
    }

    fn take_tx(&mut self) -> Option<BTxQueue> {
        take_flag(&self.tx_taken)?;
        Some(Box::new(TxQueue {
            inner: self.control.tx_queue(&self.shared),
            taken: self.tx_taken.clone(),
        }))
    }

    fn take_rx(&mut self) -> Option<BRxQueue> {
        take_flag(&self.rx_taken)?;
        Some(Box::new(RxQueue {
            inner: self.control.rx_queue(&self.shared),
            taken: self.rx_taken.clone(),
        }))
    }

    fn take_irq_handler(&mut self) -> Option<BIrqHandler> {
        take_flag(&self.irq_taken)?;
        Some(Box::new(IrqHandler {
            inner: self.control.irq_handler(&self.shared),
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

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

pub struct TxQueue<T: TTxQueue> {
    inner: T,
    taken: Arc<AtomicBool>,
}

impl<T: TTxQueue> Drop for TxQueue<T> {
    fn drop(&mut self) {
        self.taken.store(false, Ordering::Release);
    }
}

impl<T: TTxQueue> TTxQueue for TxQueue<T> {
    fn base_addr(&self) -> usize {
        self.inner.base_addr()
    }

    fn poll(&mut self) -> crate::SerialEvent {
        self.inner.poll()
    }

    fn try_write(&mut self, bytes: &[u8]) -> usize {
        self.inner.try_write(bytes)
    }
}

pub struct RxQueue<T: TRxQueue> {
    inner: T,
    taken: Arc<AtomicBool>,
}

impl<T: TRxQueue> Drop for RxQueue<T> {
    fn drop(&mut self) {
        self.taken.store(false, Ordering::Release);
    }
}

impl<T: TRxQueue> TRxQueue for RxQueue<T> {
    fn base_addr(&self) -> usize {
        self.inner.base_addr()
    }

    fn poll(&mut self) -> crate::SerialEvent {
        self.inner.poll()
    }

    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        self.inner.try_read(bytes)
    }
}

pub struct IrqHandler<T: TIrqHandler> {
    inner: T,
    taken: Arc<AtomicBool>,
}

impl<T: TIrqHandler> Drop for IrqHandler<T> {
    fn drop(&mut self) {
        self.taken.store(false, Ordering::Release);
    }
}

impl<T: TIrqHandler> TIrqHandler for IrqHandler<T> {
    fn base_addr(&self) -> usize {
        self.inner.base_addr()
    }

    fn handle_irq(&self) -> crate::SerialEvent {
        self.inner.handle_irq()
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
