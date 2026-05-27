use alloc::{boxed::Box, sync::Arc};
use core::{cell::UnsafeCell, num::NonZeroU32};

use heapless::Deque;
use rdif_base::DriverGeneric;
use spin::Mutex;

use super::{
    BIrqHandler, BReciever, BSender, BSerial, InterfaceRaw, InterruptMask, TransBytesError,
};
use crate::TransferError;

pub struct SerialDyn<T: InterfaceRaw> {
    inner: T,
    tx: Arc<Mutex<Option<BSender>>>,

    rx: Arc<Mutex<Option<Arc<SRecv>>>>,
    irq_handler: Arc<Mutex<Option<BIrqHandler>>>,

    rx_clone: Arc<SRecv>,
}

impl<T: InterfaceRaw> SerialDyn<T> {
    fn _new(mut inner: T) -> Self {
        let tx = inner.take_tx().unwrap();
        let tx: BSender = Box::new(tx);
        let tx = Arc::new(Mutex::new(Some(tx)));

        let rx_inner = inner.take_rx().unwrap();
        let rx_inner = SRecv(UnsafeCell::new(RecieverInner {
            inner: Box::new(rx_inner),
            fifo: RcvBuff(Deque::new()),
        }));
        let rx_inner = Arc::new(rx_inner);
        let srcv = rx_inner.clone();
        let rx = Arc::new(Mutex::new(Some(rx_inner)));

        let irq_inner = inner.irq_handler().unwrap();

        let irq_inner: BIrqHandler = Box::new(irq_inner);
        let irq_handler = Arc::new(Mutex::new(Some(irq_inner)));
        Self {
            inner,
            tx,
            rx,
            irq_handler,
            rx_clone: srcv,
        }
    }

    pub fn new_boxed(inner: T) -> BSerial {
        Box::new(Self::_new(inner)) as _
    }
}

impl<T: InterfaceRaw> super::Interface for SerialDyn<T> {
    fn base_addr(&self) -> usize {
        self.inner.base_addr()
    }

    fn take_tx(&mut self) -> Option<Box<dyn super::TSender>> {
        let tx = self.tx.lock().take()?;
        Some(Box::new(Sender {
            c: self.tx.clone(),
            inner: Some(tx),
        }))
    }

    fn take_rx(&mut self) -> Option<Box<dyn super::TReciever>> {
        let rx = self.rx.lock().take()?;
        Some(Box::new(Reciever {
            c: self.rx.clone(),
            inner: Some(rx),
        }))
    }

    fn irq_handler(&mut self) -> Option<Box<dyn super::TIrqHandler>> {
        let h = self.irq_handler.lock().take()?;
        Some(Box::new(IrqHandler {
            c: self.irq_handler.clone(),
            inner: Some(h),
            rcv: self.rx_clone.clone(),
        }))
    }

    fn set_config(&mut self, config: &crate::Config) -> Result<(), crate::ConfigError> {
        self.inner.set_config(config)
    }

    fn baudrate(&self) -> u32 {
        self.inner.baudrate()
    }

    fn data_bits(&self) -> crate::DataBits {
        self.inner.data_bits()
    }

    fn stop_bits(&self) -> crate::StopBits {
        self.inner.stop_bits()
    }

    fn parity(&self) -> crate::Parity {
        self.inner.parity()
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        self.inner.clock_freq()
    }

    fn enable_loopback(&mut self) {
        self.inner.enable_loopback()
    }

    fn disable_loopback(&mut self) {
        self.inner.disable_loopback()
    }

    fn is_loopback_enabled(&self) -> bool {
        self.inner.is_loopback_enabled()
    }

    fn enable_interrupts(&mut self, mask: InterruptMask) {
        let mut val = self.inner.get_irq_mask();
        val |= mask;
        self.inner.set_irq_mask(val);
    }

    fn disable_interrupts(&mut self, mask: InterruptMask) {
        let mut val = self.inner.get_irq_mask();
        val &= !mask;
        self.inner.set_irq_mask(val);
    }

    fn get_enabled_interrupts(&self) -> InterruptMask {
        self.inner.get_irq_mask()
    }
}

impl<T: InterfaceRaw> DriverGeneric for SerialDyn<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(&self.inner)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(&mut self.inner)
    }
}

pub struct Sender {
    c: Arc<Mutex<Option<BSender>>>,
    inner: Option<BSender>,
}

impl Drop for Sender {
    fn drop(&mut self) {
        let mut guard = self.c.lock();
        guard.replace(self.inner.take().unwrap());
    }
}

impl super::TSender for Sender {
    fn write_byte(&mut self, byte: u8) -> bool {
        let s = self.inner.as_mut().unwrap();
        s.write_byte(byte)
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> usize {
        let s = self.inner.as_mut().unwrap();
        s.write_bytes(bytes)
    }
}

struct RecieverInner {
    inner: BReciever,
    fifo: RcvBuff,
}

pub struct Reciever {
    c: Arc<Mutex<Option<Arc<SRecv>>>>,
    inner: Option<Arc<SRecv>>,
}

impl Reciever {
    fn inner(&self) -> &SRecv {
        self.inner.as_ref().unwrap()
    }
}

impl super::TReciever for Reciever {
    fn read_byte(&mut self) -> Option<Result<u8, TransferError>> {
        if let Some(b) = self.inner().fifo_pop() {
            return Some(b);
        }

        self.inner().read_byte()
    }

    fn read_bytes(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let recv = self.inner();
        let mut n = 0;

        // 先从 FIFO 读取尽可能多的数据
        while n < bytes.len() {
            match recv.fifo_pop() {
                Some(Ok(b)) => {
                    bytes[n] = b;
                    n += 1;
                }
                Some(Err(e)) => {
                    return Err(TransBytesError {
                        bytes_transferred: n,
                        kind: e,
                    });
                }
                None => break,
            }
        }

        // 如果已经填满则返回
        if n == bytes.len() {
            return Ok(n);
        }

        // FIFO 没有更多数据时，再批量从底层读取
        match recv.read_bytes(&mut bytes[n..]) {
            Ok(m) => Ok(n + m),
            Err(e) => Err(TransBytesError {
                bytes_transferred: n + e.bytes_transferred,
                kind: e.kind,
            }),
        }
    }
}

impl Drop for Reciever {
    fn drop(&mut self) {
        let mut guard = self.c.lock();
        guard.replace(self.inner.take().unwrap());
    }
}

pub struct IrqHandler {
    c: Arc<Mutex<Option<BIrqHandler>>>,
    inner: Option<BIrqHandler>,
    rcv: Arc<SRecv>,
}

impl super::TIrqHandler for IrqHandler {
    fn clean_interrupt_status(&self) -> InterruptMask {
        let h = self.inner.as_ref().unwrap();
        let status = h.clean_interrupt_status();
        if status.contains(InterruptMask::RX_AVAILABLE) {
            while let Some(b) = self.rcv.read_byte() {
                self.rcv.fifo_push(b);
            }
        }

        status
    }
}

impl Drop for IrqHandler {
    fn drop(&mut self) {
        let mut guard = self.c.lock();
        guard.replace(self.inner.take().unwrap());
    }
}

#[repr(align(64))]
struct RcvBuff(Deque<Result<u8, TransferError>, 64>);

struct SRecv(UnsafeCell<RecieverInner>);

unsafe impl Send for SRecv {}
unsafe impl Sync for SRecv {}

impl SRecv {
    fn fifo_push(&self, byte: Result<u8, TransferError>) {
        let inner = unsafe { &mut *self.0.get() };
        let _ = inner.fifo.0.push_back(byte);
    }

    fn fifo_pop(&self) -> Option<Result<u8, TransferError>> {
        let inner = unsafe { &mut *self.0.get() };
        inner.fifo.0.pop_front()
    }

    fn read_byte(&self) -> Option<Result<u8, TransferError>> {
        let inner = unsafe { &mut *self.0.get() };
        inner.inner.read_byte()
    }

    fn read_bytes(&self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let inner = unsafe { &mut *self.0.get() };
        inner.inner.read_bytes(bytes)
    }
}
