use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    cell::UnsafeCell,
    num::NonZeroU32,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};

use rdif_base::DriverGeneric;

use super::{
    BIrqHandler, BRxQueue, BSerial, BTxQueue, InterfaceRaw, InterruptMask, SetBackError,
    TIrqHandler, TRxQueue, TTxQueue, TransBytesError, TransferError,
};

const BORROW_IDLE: u8 = 0;
const BORROW_TX: u8 = 1;
const BORROW_RX: u8 = 2;
const BORROW_IRQ: u8 = 3;
const BORROW_CONTROL: u8 = 4;

const TX_EVENT_MASK: crate::SerialEvent =
    crate::SerialEvent::TX_READY.union(crate::SerialEvent::TX_ERROR);
const RX_EVENT_MASK: crate::SerialEvent = crate::SerialEvent::RX_READY
    .union(crate::SerialEvent::RX_ERROR)
    .union(crate::SerialEvent::OVERRUN);
const RX_IRQ_BUFFER_CAP: usize = 4096;

pub struct SerialDyn<T: InterfaceRaw> {
    name: String,
    base_addr: usize,
    core: Arc<SerialCore<T>>,
    tx: Arc<TxShared<T>>,
    rx: Arc<RxShared<T>>,
    tx_taken: Arc<AtomicBool>,
    rx_taken: Arc<AtomicBool>,
    irq_taken: Arc<AtomicBool>,
}

impl<T: InterfaceRaw> SerialDyn<T> {
    pub fn new_boxed(control: T) -> BSerial {
        let name = String::from(control.name());
        let base_addr = control.base_addr();
        let core = Arc::new(SerialCore::new(control));
        let tx = Arc::new(TxShared::new(core.clone()));
        let rx = Arc::new(RxShared::new(core.clone()));
        Box::new(Self {
            name,
            base_addr,
            core,
            tx,
            rx,
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
        self.core
            .with_raw(BORROW_CONTROL, |control| control.set_config(config))
            .unwrap_or(Err(crate::ConfigError::Timeout))
    }

    fn baudrate(&self) -> u32 {
        self.core
            .with_raw(BORROW_CONTROL, |control| control.baudrate())
            .unwrap_or(0)
    }

    fn data_bits(&self) -> crate::DataBits {
        self.core
            .with_raw(BORROW_CONTROL, |control| control.data_bits())
            .unwrap_or(crate::DataBits::Eight)
    }

    fn stop_bits(&self) -> crate::StopBits {
        self.core
            .with_raw(BORROW_CONTROL, |control| control.stop_bits())
            .unwrap_or(crate::StopBits::One)
    }

    fn parity(&self) -> crate::Parity {
        self.core
            .with_raw(BORROW_CONTROL, |control| control.parity())
            .unwrap_or(crate::Parity::None)
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        self.core
            .with_raw(BORROW_CONTROL, |control| control.clock_freq())
            .unwrap_or(None)
    }

    fn open(&mut self) {
        let _ = self.core.with_raw(BORROW_CONTROL, |control| control.open());
    }

    fn close(&mut self) {
        let _ = self
            .core
            .with_raw(BORROW_CONTROL, |control| control.close());
    }

    fn enable_loopback(&mut self) {
        let _ = self
            .core
            .with_raw(BORROW_CONTROL, |control| control.enable_loopback());
    }

    fn disable_loopback(&mut self) {
        let _ = self
            .core
            .with_raw(BORROW_CONTROL, |control| control.disable_loopback());
    }

    fn is_loopback_enabled(&self) -> bool {
        self.core
            .with_raw(BORROW_CONTROL, |control| control.is_loopback_enabled())
            .unwrap_or(false)
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.core.set_pending_irq_mask(mask);
        let _ = self.core.with_raw(BORROW_CONTROL, |_| ());
    }

    fn get_irq_mask(&self) -> InterruptMask {
        let mask = self
            .core
            .with_raw(BORROW_CONTROL, |control| control.get_irq_mask())
            .unwrap_or_else(|| self.core.irq_mask());
        self.core.set_irq_mask(mask);
        mask
    }

    fn take_tx(&mut self) -> Option<BTxQueue> {
        take_flag(&self.tx_taken)?;
        Some(Box::new(TxQueue {
            base_addr: self.base_addr,
            tx: self.tx.clone(),
            taken: self.tx_taken.clone(),
        }))
    }

    fn take_rx(&mut self) -> Option<BRxQueue> {
        take_flag(&self.rx_taken)?;
        Some(Box::new(RxQueue {
            base_addr: self.base_addr,
            rx: self.rx.clone(),
            taken: self.rx_taken.clone(),
        }))
    }

    fn take_irq_handler(&mut self) -> Option<BIrqHandler> {
        take_flag(&self.irq_taken)?;
        Some(Box::new(IrqHandler {
            base_addr: self.base_addr,
            core: self.core.clone(),
            tx: self.tx.clone(),
            rx: self.rx.clone(),
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

struct SerialCore<T: InterfaceRaw> {
    raw: UnsafeCell<T>,
    borrow: AtomicU8,
    irq_mask: AtomicU32,
    irq_mask_dirty: AtomicBool,
    pending_irq: AtomicBool,
}

// SAFETY: `SerialCore` is the only place that creates `&mut T` from the raw
// `UnsafeCell`. The atomic borrow gate guarantees at most one mutable access in
// TX/RX/IRQ/control paths, so raw drivers only need their normal `Send` bound.
unsafe impl<T: InterfaceRaw> Sync for SerialCore<T> {}

impl<T: InterfaceRaw> SerialCore<T> {
    fn new(raw: T) -> Self {
        Self {
            raw: UnsafeCell::new(raw),
            borrow: AtomicU8::new(BORROW_IDLE),
            irq_mask: AtomicU32::new(0),
            irq_mask_dirty: AtomicBool::new(false),
            pending_irq: AtomicBool::new(false),
        }
    }

    fn with_borrow<R>(&self, borrower: u8, f: impl FnOnce() -> R) -> Option<R> {
        if self
            .borrow
            .compare_exchange(BORROW_IDLE, borrower, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }

        let _guard = BorrowGuard {
            borrow: &self.borrow,
        };
        let result = f();
        Some(result)
    }

    fn with_raw<R>(&self, borrower: u8, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.with_borrow(borrower, || {
            let raw = unsafe { &mut *self.raw.get() };
            self.apply_pending_irq_mask(raw);
            f(raw)
        })
    }

    fn set_irq_mask(&self, mask: InterruptMask) {
        self.irq_mask.store(mask.bits(), Ordering::Release);
    }

    fn set_pending_irq_mask(&self, mask: InterruptMask) {
        self.set_irq_mask(mask);
        self.irq_mask_dirty.store(true, Ordering::Release);
    }

    fn apply_pending_irq_mask(&self, raw: &mut T) {
        if self.irq_mask_dirty.swap(false, Ordering::AcqRel) {
            raw.set_irq_mask(self.irq_mask());
        }
    }

    fn irq_mask(&self) -> InterruptMask {
        InterruptMask::from_bits_retain(self.irq_mask.load(Ordering::Acquire))
    }

    fn set_pending_irq(&self) {
        self.pending_irq.store(true, Ordering::Release);
    }

    fn take_pending_irq(&self) -> bool {
        self.pending_irq.swap(false, Ordering::AcqRel)
    }
}

struct BorrowGuard<'a> {
    borrow: &'a AtomicU8,
}

impl Drop for BorrowGuard<'_> {
    fn drop(&mut self) {
        self.borrow.store(BORROW_IDLE, Ordering::Release);
    }
}

struct DirectionState {
    status: AtomicU32,
    mask: crate::SerialEvent,
}

impl DirectionState {
    const fn new(mask: crate::SerialEvent) -> Self {
        Self {
            status: AtomicU32::new(0),
            mask,
        }
    }

    fn record(&self, event: crate::SerialEvent) {
        let event = event & self.mask;
        if !event.is_empty() {
            self.status.fetch_or(event.bits(), Ordering::AcqRel);
        }
    }

    fn poll(&self) -> crate::SerialEvent {
        crate::SerialEvent::from_bits_retain(self.status.load(Ordering::Acquire)) & self.mask
    }

    fn take(&self) -> crate::SerialEvent {
        crate::SerialEvent::from_bits_retain(
            self.status.fetch_and(!self.mask.bits(), Ordering::AcqRel),
        ) & self.mask
    }

    fn set(&self, event: crate::SerialEvent) {
        self.status
            .store((event & self.mask).bits(), Ordering::Release);
    }
}

struct TxShared<T: InterfaceRaw> {
    core: Arc<SerialCore<T>>,
    state: DirectionState,
}

impl<T: InterfaceRaw> TxShared<T> {
    fn new(core: Arc<SerialCore<T>>) -> Self {
        Self {
            core,
            state: DirectionState::new(TX_EVENT_MASK),
        }
    }

    fn record(&self, event: crate::SerialEvent) {
        self.state.record(event);
    }
}

struct RxShared<T: InterfaceRaw> {
    core: Arc<SerialCore<T>>,
    state: DirectionState,
    ring: UnsafeCell<RxRing>,
}

// SAFETY: `RxShared::ring` is only accessed while holding the matching
// `SerialCore` borrow gate in the RX or IRQ path. The gate excludes task RX
// reads from hard-IRQ RX fills without requiring a mutex in the IRQ callback.
unsafe impl<T: InterfaceRaw> Sync for RxShared<T> {}

impl<T: InterfaceRaw> RxShared<T> {
    fn new(core: Arc<SerialCore<T>>) -> Self {
        Self {
            core,
            state: DirectionState::new(RX_EVENT_MASK),
            ring: UnsafeCell::new(RxRing::new()),
        }
    }

    fn record(&self, event: crate::SerialEvent) {
        self.state.record(event);
    }

    fn push_irq_item(&self, item: Result<u8, TransferError>) -> bool {
        let pushed = unsafe { &mut *self.ring.get() }.push_back(item);
        self.refresh_ring_state();
        pushed
    }

    fn pop_item(&self) -> Option<Result<u8, TransferError>> {
        let item = unsafe { &mut *self.ring.get() }.pop_front();
        self.refresh_ring_state();
        item
    }

    fn refresh_ring_state(&self) {
        self.state.set(unsafe { &*self.ring.get() }.event());
    }
}

#[derive(Clone, Copy)]
struct RxItem(Option<Result<u8, TransferError>>);

impl RxItem {
    const EMPTY: Self = Self(None);
}

struct RxRing {
    buf: [RxItem; RX_IRQ_BUFFER_CAP],
    head: usize,
    len: usize,
    rx_error_len: usize,
    overrun_len: usize,
}

impl RxRing {
    const fn new() -> Self {
        Self {
            buf: [RxItem::EMPTY; RX_IRQ_BUFFER_CAP],
            head: 0,
            len: 0,
            rx_error_len: 0,
            overrun_len: 0,
        }
    }

    fn push_back(&mut self, item: Result<u8, TransferError>) -> bool {
        if self.len == RX_IRQ_BUFFER_CAP {
            return false;
        }
        if let Err(kind) = item {
            self.rx_error_len += 1;
            if matches!(kind, TransferError::Overrun(_)) {
                self.overrun_len += 1;
            }
        }
        let tail = (self.head + self.len) % RX_IRQ_BUFFER_CAP;
        self.buf[tail] = RxItem(Some(item));
        self.len += 1;
        true
    }

    fn pop_front(&mut self) -> Option<Result<u8, TransferError>> {
        if self.len == 0 {
            return None;
        }
        let item = self.buf[self.head]
            .0
            .take()
            .expect("ring slot must contain an item while len is non-zero");
        if let Err(kind) = item {
            self.rx_error_len -= 1;
            if matches!(kind, TransferError::Overrun(_)) {
                self.overrun_len -= 1;
            }
        }
        self.head = (self.head + 1) % RX_IRQ_BUFFER_CAP;
        self.len -= 1;
        Some(item)
    }

    fn event(&self) -> crate::SerialEvent {
        let mut event = crate::SerialEvent::empty();
        if self.len > 0 {
            event |= crate::SerialEvent::RX_READY;
        }
        if self.rx_error_len > 0 {
            event |= crate::SerialEvent::RX_ERROR;
        }
        if self.overrun_len > 0 {
            event |= crate::SerialEvent::OVERRUN;
        }
        event
    }
}

fn record_event<T: InterfaceRaw>(
    tx: &TxShared<T>,
    rx: &RxShared<T>,
    event: crate::SerialEvent,
) -> crate::SerialEvent {
    tx.record(event);
    rx.record(event);
    event
}

pub struct TxQueue<T: InterfaceRaw> {
    base_addr: usize,
    tx: Arc<TxShared<T>>,
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
        self.tx.state.poll()
    }

    fn try_write(&mut self, bytes: &[u8]) -> usize {
        let Some(&byte) = bytes.first() else {
            return 0;
        };
        let status = self.tx.state.take();
        if !status.tx_ready() {
            self.tx.record(status);
            return 0;
        }
        if self
            .tx
            .core
            .with_raw(BORROW_TX, |control| control.write_byte(byte))
            .is_none()
        {
            self.tx.record(status);
            return 0;
        }
        1
    }
}

pub struct RxQueue<T: InterfaceRaw> {
    base_addr: usize,
    rx: Arc<RxShared<T>>,
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
        self.rx.state.poll()
    }

    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        if bytes.is_empty() {
            return Ok(0);
        };
        self.rx
            .core
            .with_borrow(BORROW_RX, || {
                let mut read = 0;
                let mut first_error = None;

                for byte in bytes {
                    let Some(result) = self.rx.pop_item() else {
                        break;
                    };
                    match result {
                        Ok(b) => {
                            *byte = b;
                            read += 1;
                        }
                        Err(TransferError::Overrun(b)) => {
                            *byte = b;
                            read += 1;
                            first_error.get_or_insert(TransferError::Overrun(b));
                        }
                        Err(kind) => {
                            first_error.get_or_insert(kind);
                        }
                    }
                }

                if let Some(kind) = first_error {
                    Err(TransBytesError {
                        bytes_transferred: read,
                        kind,
                    })
                } else {
                    Ok(read)
                }
            })
            .unwrap_or(Ok(0))
    }
}

pub struct IrqHandler<T: InterfaceRaw> {
    base_addr: usize,
    core: Arc<SerialCore<T>>,
    tx: Arc<TxShared<T>>,
    rx: Arc<RxShared<T>>,
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
        if self.core.take_pending_irq() {
            return self.handle_pending_irq();
        }
        self.handle_current_irq()
    }
}

impl<T: InterfaceRaw> IrqHandler<T> {
    fn handle_current_irq(&self) -> crate::SerialEvent {
        if let Some(event) = self.core.with_borrow(BORROW_IRQ, || {
            let control = unsafe { &mut *self.core.raw.get() };
            self.core.apply_pending_irq_mask(control);
            let event = control.handle_irq();
            self.handle_raw_irq(control, event)
        }) {
            record_event(&self.tx, &self.rx, event)
        } else {
            self.core.set_pending_irq();
            self.tx.state.poll() | self.rx.state.poll()
        }
    }

    fn handle_pending_irq(&self) -> crate::SerialEvent {
        if let Some(event) = self.core.with_borrow(BORROW_IRQ, || {
            let control = unsafe { &mut *self.core.raw.get() };
            self.core.apply_pending_irq_mask(control);
            let event = control.handle_irq();
            self.handle_raw_irq(control, event)
        }) {
            record_event(&self.tx, &self.rx, event)
        } else {
            self.core.set_pending_irq();
            self.tx.state.poll() | self.rx.state.poll()
        }
    }

    fn handle_raw_irq(
        &self,
        control: &mut T,
        first_event: crate::SerialEvent,
    ) -> crate::SerialEvent {
        let mut event = crate::SerialEvent::empty();
        let mut current = first_event;

        loop {
            event |= current;
            if !current.intersects(RX_EVENT_MASK) {
                break;
            }

            let Some(item) = control.read_byte(current) else {
                break;
            };
            if !self.rx.push_irq_item(item) {
                event |= crate::SerialEvent::RX_ERROR | crate::SerialEvent::OVERRUN;
                break;
            }

            current = control.handle_irq();
        }

        event | self.rx.state.poll()
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

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        mem::size_of,
        num::NonZeroU32,
        sync::atomic::{AtomicBool, AtomicU32},
    };

    use super::*;
    use crate::Interface;

    struct LayoutRaw;

    impl InterfaceRaw for LayoutRaw {
        fn name(&self) -> &str {
            "layout raw"
        }

        fn base_addr(&self) -> usize {
            0
        }

        fn set_config(&mut self, _config: &crate::Config) -> Result<(), crate::ConfigError> {
            Ok(())
        }

        fn baudrate(&self) -> u32 {
            115_200
        }

        fn data_bits(&self) -> crate::DataBits {
            crate::DataBits::Eight
        }

        fn stop_bits(&self) -> crate::StopBits {
            crate::StopBits::One
        }

        fn parity(&self) -> crate::Parity {
            crate::Parity::None
        }

        fn clock_freq(&self) -> Option<NonZeroU32> {
            NonZeroU32::new(1)
        }

        fn open(&mut self) {}

        fn close(&mut self) {}

        fn enable_loopback(&mut self) {}

        fn disable_loopback(&mut self) {}

        fn is_loopback_enabled(&self) -> bool {
            false
        }

        fn set_irq_mask(&mut self, _mask: InterruptMask) {}

        fn get_irq_mask(&self) -> InterruptMask {
            InterruptMask::empty()
        }

        fn poll_status(&mut self) -> crate::SerialEvent {
            crate::SerialEvent::empty()
        }

        fn write_byte(&mut self, _byte: u8) {}

        fn read_byte(&mut self, _status: crate::SerialEvent) -> Option<Result<u8, TransferError>> {
            None
        }

        fn handle_irq(&mut self) -> crate::SerialEvent {
            crate::SerialEvent::empty()
        }
    }

    #[test]
    fn queues_hold_only_their_own_direction_shared_state() {
        assert_eq!(
            size_of::<TxQueue<LayoutRaw>>(),
            size_of::<usize>()
                + size_of::<Arc<TxShared<LayoutRaw>>>()
                + size_of::<Arc<AtomicBool>>(),
        );
        assert_eq!(
            size_of::<RxQueue<LayoutRaw>>(),
            size_of::<usize>()
                + size_of::<Arc<RxShared<LayoutRaw>>>()
                + size_of::<Arc<AtomicBool>>(),
        );
    }

    struct MaskRaw {
        applied_mask: Arc<AtomicU32>,
    }

    impl InterfaceRaw for MaskRaw {
        fn name(&self) -> &str {
            "mask raw"
        }

        fn base_addr(&self) -> usize {
            0
        }

        fn set_config(&mut self, _config: &crate::Config) -> Result<(), crate::ConfigError> {
            Ok(())
        }

        fn baudrate(&self) -> u32 {
            115_200
        }

        fn data_bits(&self) -> crate::DataBits {
            crate::DataBits::Eight
        }

        fn stop_bits(&self) -> crate::StopBits {
            crate::StopBits::One
        }

        fn parity(&self) -> crate::Parity {
            crate::Parity::None
        }

        fn clock_freq(&self) -> Option<NonZeroU32> {
            NonZeroU32::new(1)
        }

        fn open(&mut self) {}

        fn close(&mut self) {}

        fn enable_loopback(&mut self) {}

        fn disable_loopback(&mut self) {}

        fn is_loopback_enabled(&self) -> bool {
            false
        }

        fn set_irq_mask(&mut self, mask: InterruptMask) {
            self.applied_mask.store(mask.bits(), Ordering::Release);
        }

        fn get_irq_mask(&self) -> InterruptMask {
            InterruptMask::from_bits_retain(self.applied_mask.load(Ordering::Acquire))
        }

        fn poll_status(&mut self) -> crate::SerialEvent {
            crate::SerialEvent::empty()
        }

        fn write_byte(&mut self, _byte: u8) {}

        fn read_byte(&mut self, _status: crate::SerialEvent) -> Option<Result<u8, TransferError>> {
            None
        }

        fn handle_irq(&mut self) -> crate::SerialEvent {
            crate::SerialEvent::empty()
        }
    }

    #[test]
    fn set_irq_mask_while_raw_busy_is_applied_on_next_raw_access() {
        let applied_mask = Arc::new(AtomicU32::new(0));
        let core = Arc::new(SerialCore::new(MaskRaw {
            applied_mask: applied_mask.clone(),
        }));
        let mut serial = SerialDyn {
            name: String::from("mask raw"),
            base_addr: 0,
            core: core.clone(),
            tx: Arc::new(TxShared::new(core.clone())),
            rx: Arc::new(RxShared::new(core.clone())),
            tx_taken: Arc::new(AtomicBool::new(false)),
            rx_taken: Arc::new(AtomicBool::new(false)),
            irq_taken: Arc::new(AtomicBool::new(false)),
        };

        assert!(
            serial
                .core
                .borrow
                .compare_exchange(BORROW_IDLE, BORROW_TX, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            "test should own the raw borrow gate"
        );

        serial.set_irq_mask(InterruptMask::TX_EMPTY);
        assert_eq!(
            applied_mask.load(Ordering::Acquire),
            0,
            "busy raw access cannot apply the hardware mask immediately"
        );

        serial.core.borrow.store(BORROW_IDLE, Ordering::Release);

        assert_eq!(serial.get_irq_mask(), InterruptMask::TX_EMPTY);
        assert_eq!(
            applied_mask.load(Ordering::Acquire),
            InterruptMask::TX_EMPTY.bits()
        );
    }
}
