#![no_std]

extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::{alloc::Layout, cell::UnsafeCell};

use dma_api::{ContiguousBuffer, ContiguousBufferPool, DeviceDma, DmaDirection, DmaOp};
use futures::task::AtomicWaker;
pub use rdif_eth::{IrqHandler as InterfaceIrqHandler, *};

fn other_error(msg: &'static str) -> NetError {
    NetError::Other(Box::new(KError::Unknown(msg)))
}

struct QueueWakerMap(UnsafeCell<BTreeMap<usize, Arc<AtomicWaker>>>);

impl QueueWakerMap {
    fn new() -> Self {
        Self(UnsafeCell::new(BTreeMap::new()))
    }

    fn register(&self, queue_id: usize) -> Arc<AtomicWaker> {
        let waker = Arc::new(AtomicWaker::new());
        unsafe { &mut *self.0.get() }.insert(queue_id, waker.clone());
        waker
    }

    fn wake(&self, queue_id: usize) {
        if let Some(waker) = unsafe { &*self.0.get() }.get(&queue_id) {
            waker.wake();
        }
    }
}

struct NetInner {
    interface: UnsafeCell<Box<dyn Interface>>,
    dma_op: &'static dyn DmaOp,
    tx_wakers: QueueWakerMap,
    rx_wakers: QueueWakerMap,
}

unsafe impl Send for NetInner {}
unsafe impl Sync for NetInner {}

struct IrqGuard<'a> {
    enabled: bool,
    inner: &'a Net,
}

impl Drop for IrqGuard<'_> {
    fn drop(&mut self) {
        if self.enabled {
            self.inner.interface().enable_irq();
        }
    }
}

pub struct Net {
    inner: Arc<NetInner>,
}

impl DriverGeneric for Net {
    fn name(&self) -> &str {
        self.interface().name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Net {
    pub fn new(interface: impl Interface, dma_op: &'static dyn DmaOp) -> Self {
        Self {
            inner: Arc::new(NetInner {
                interface: UnsafeCell::new(Box::new(interface)),
                dma_op,
                tx_wakers: QueueWakerMap::new(),
                rx_wakers: QueueWakerMap::new(),
            }),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn interface(&self) -> &mut dyn Interface {
        unsafe { &mut **self.inner.interface.get() }
    }

    fn irq_guard(&self) -> IrqGuard<'_> {
        let enabled = self.interface().is_irq_enabled();
        if enabled {
            self.interface().disable_irq();
        }
        IrqGuard {
            enabled,
            inner: self,
        }
    }

    pub fn mac_address(&self) -> [u8; 6] {
        self.interface().mac_address()
    }

    /// Access the device's optional wireless control plane.
    ///
    /// Returns `None` for a plain wired NIC. Forwards to
    /// [`Interface::wifi_control`] so the upper layers can drive a wireless
    /// device (STA/SoftAP control, link policy, RX wake) through the same net
    /// device handle as any other NIC.
    #[allow(clippy::mut_from_ref)]
    pub fn wifi_control(&self) -> Option<&mut dyn WifiControl> {
        self.interface().wifi_control()
    }

    pub fn enable_irq(&mut self) {
        self.interface().enable_irq();
    }

    pub fn disable_irq(&mut self) {
        self.interface().disable_irq();
    }

    pub fn is_irq_enabled(&self) -> bool {
        self.interface().is_irq_enabled()
    }

    pub fn create_tx_queue(&mut self) -> Result<TxQueue, NetError> {
        let irq_guard = self.irq_guard();
        let queue = self
            .interface()
            .create_tx_queue()
            .ok_or_else(|| other_error("failed to create tx queue"))?;
        let config = queue.config();
        let pool = make_pool(self.inner.dma_op, config, DmaDirection::ToDevice)?;
        let waker = self.inner.tx_wakers.register(queue.id());
        drop(irq_guard);

        Ok(TxQueue {
            interface: queue,
            pool,
            inflight: BTreeMap::new(),
            config,
            _waker: waker,
        })
    }

    pub fn create_rx_queue(&mut self) -> Result<RxQueue, NetError> {
        let irq_guard = self.irq_guard();
        let queue = self
            .interface()
            .create_rx_queue()
            .ok_or_else(|| other_error("failed to create rx queue"))?;
        let config = queue.config();
        let pool = make_pool(self.inner.dma_op, config, DmaDirection::FromDevice)?;
        let waker = self.inner.rx_wakers.register(queue.id());
        drop(irq_guard);

        let mut rx = RxQueue {
            interface: queue,
            pool,
            inflight: BTreeMap::new(),
            config,
            _waker: waker,
        };
        rx.prefill()?;
        Ok(rx)
    }

    pub fn take_irq_handler(&mut self) -> Option<IrqHandler> {
        let irq_guard = self.irq_guard();
        let handler = self.interface().take_irq_handler();
        drop(irq_guard);

        handler.map(|handler| IrqHandler {
            inner: self.inner.clone(),
            handler,
        })
    }

    /// Detaches a standalone control-plane handle for this device.
    ///
    /// Returns `None` for a plain wired NIC. The handle clones the same
    /// `Arc<NetInner>` the data plane uses (like [`Net::irq_handler`]), so the
    /// control plane (STA/SoftAP switch, link policy) stays reachable *after*
    /// `Net` is consumed into a driver. Used to drive runtime Wi-Fi mode
    /// switching from a separate task/syscall context.
    pub fn wifi_control_handle(&self) -> Option<WifiControlHandle> {
        self.wifi_control().is_some().then(|| WifiControlHandle {
            inner: self.inner.clone(),
        })
    }
}

/// Standalone handle to a device's wireless control plane.
///
/// Holds a clone of the device's `Arc<NetInner>`, so it keeps working after the
/// originating [`Net`] has been consumed into a data-plane driver. See
/// [`Net::wifi_control_handle`].
pub struct WifiControlHandle {
    inner: Arc<NetInner>,
}

impl Clone for WifiControlHandle {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

unsafe impl Send for WifiControlHandle {}
unsafe impl Sync for WifiControlHandle {}

impl WifiControlHandle {
    /// Access the wireless control plane.
    ///
    /// # Safety / concurrency
    ///
    /// This aliases the same `Interface` the data plane drives. The caller must
    /// not invoke control operations concurrently with the device's RX/TX or
    /// poll path on the same interface. In practice mode switching is issued
    /// from a syscall/task context that is serialized against the stack's poll
    /// task, never from inside an RX callback.
    #[allow(clippy::mut_from_ref)]
    pub fn wifi_control(&self) -> Option<&mut dyn WifiControl> {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.wifi_control()
    }

    /// The device's current MAC address (may change across a mode switch as the
    /// firmware re-creates its VIF).
    pub fn mac_address(&self) -> [u8; 6] {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.mac_address()
    }
}

fn make_pool(
    dma_op: &'static dyn DmaOp,
    config: QueueConfig,
    direction: DmaDirection,
) -> Result<ContiguousBufferPool, NetError> {
    let layout = Layout::from_size_align(config.buf_size, config.align.max(1))
        .map_err(|_| other_error("invalid queue layout"))?;
    let dma = DeviceDma::new_identity(config.dma_mask, dma_op);
    Ok(dma.contiguous_buffer_pool(layout, direction, config.ring_size))
}

pub struct IrqHandler {
    inner: Arc<NetInner>,
    handler: rdif_eth::BIrqHandler,
}

unsafe impl Send for IrqHandler {}
unsafe impl Sync for IrqHandler {}

impl IrqHandler {
    pub fn enable(&self) {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.enable_irq();
    }

    pub fn disable(&self) {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.disable_irq();
    }

    /// Handles a device interrupt and returns queue events without waking task
    /// wakers.
    ///
    /// This is the IRQ top-half entry: it only asks the portable driver to
    /// identify/acknowledge the interrupt source and publish queue event bits.
    /// Runtime queue wakers must be invoked later from task/deferred context
    /// through [`handle`](Self::handle).
    pub fn handle_irq(&mut self) -> rdif_eth::Event {
        self.handler.handle_irq()
    }

    /// Handles a device interrupt and wakes registered queue waiters.
    ///
    /// Use this only from task/deferred context. Hard IRQ callbacks should call
    /// [`handle_irq`](Self::handle_irq) and defer waker execution.
    pub fn handle(&mut self) {
        let event = self.handle_irq();
        for id in event.tx_queue.iter() {
            self.inner.tx_wakers.wake(id);
        }
        for id in event.rx_queue.iter() {
            self.inner.rx_wakers.wake(id);
        }
    }

    pub fn enable_irq(&self) {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.enable_irq();
    }

    pub fn disable_irq(&self) {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.disable_irq();
    }

    pub fn is_irq_enabled(&self) -> bool {
        let iface = unsafe { &mut **self.inner.interface.get() };
        iface.is_irq_enabled()
    }
}

pub struct TxQueue {
    interface: Box<dyn ITxQueue>,
    pool: ContiguousBufferPool,
    inflight: BTreeMap<u64, ContiguousBuffer>,
    config: QueueConfig,
    _waker: Arc<AtomicWaker>,
}

impl TxQueue {
    fn capacity(&self) -> usize {
        self.config.ring_size.saturating_sub(1)
    }

    fn reclaim_bounded(&mut self, limit: usize) -> Result<usize, NetError> {
        let mut reclaimed = 0;
        while reclaimed < limit {
            let Some(bus_addr) = self.interface.reclaim() else {
                break;
            };
            let Some(buff) = self.inflight.remove(&bus_addr) else {
                return Err(other_error("reclaimed unknown tx buffer"));
            };
            drop(buff);
            reclaimed += 1;
        }
        Ok(reclaimed)
    }

    pub fn id(&self) -> usize {
        self.interface.id()
    }

    pub fn buf_size(&self) -> usize {
        self.config.buf_size
    }

    pub fn prepare_send<R>(
        &mut self,
        len: usize,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<(R, TxPending<'_>), NetError> {
        if len > self.config.buf_size {
            return Err(other_error("tx packet too large"));
        }

        self.reclaim_bounded(self.capacity().max(1))?;

        let mut buff = self.pool.alloc()?;
        let bus_addr = buff.dma_addr().as_u64();
        let ret = buff.write_with_cpu(len, f);
        Ok((
            ret,
            TxPending {
                queue: self,
                len,
                bus_addr,
                buff: Some(buff),
            },
        ))
    }
}

pub struct TxPending<'a> {
    queue: &'a mut TxQueue,
    len: usize,
    bus_addr: u64,
    buff: Option<ContiguousBuffer>,
}

impl TxPending<'_> {
    pub fn bus_addr(&self) -> u64 {
        self.bus_addr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn try_submit(&mut self) -> Result<(), NetError> {
        self.queue.reclaim_bounded(self.queue.capacity().max(1))?;
        let buff = self
            .buff
            .as_ref()
            .expect("tx pending buffer should exist until submit succeeds");
        buff.prepare_for_device(0, self.len);
        self.queue.interface.submit(DmaBuffer {
            virt: buff.as_ptr(),
            bus_addr: self.bus_addr,
            len: self.len,
        })?;
        let buff = self
            .buff
            .take()
            .expect("tx pending buffer should exist until submit succeeds");
        self.queue.inflight.insert(self.bus_addr, buff);
        Ok(())
    }
}

pub struct RxQueue {
    interface: Box<dyn IRxQueue>,
    pool: ContiguousBufferPool,
    inflight: BTreeMap<u64, ContiguousBuffer>,
    config: QueueConfig,
    _waker: Arc<AtomicWaker>,
}

impl RxQueue {
    fn capacity(&self) -> usize {
        self.config.ring_size.saturating_sub(1)
    }

    fn prefill(&mut self) -> Result<(), NetError> {
        while self.inflight.len() < self.capacity() {
            let buff = self.pool.alloc()?;
            if let Err(err) = self.submit_buffer(buff) {
                if matches!(err, NetError::Retry) {
                    break;
                }
                return Err(err);
            }
        }
        Ok(())
    }

    fn submit_buffer(&mut self, buff: ContiguousBuffer) -> Result<(), NetError> {
        let bus_addr = buff.dma_addr().as_u64();
        let len = self.config.buf_size.min(buff.len());
        buff.prepare_for_device(0, len);
        self.interface.submit(DmaBuffer {
            virt: buff.as_ptr(),
            bus_addr,
            len,
        })?;
        self.inflight.insert(bus_addr, buff);
        Ok(())
    }

    fn reclaim_packet(&mut self) -> Result<Option<(ContiguousBuffer, usize)>, NetError> {
        let Some((bus_addr, len)) = self.interface.reclaim() else {
            return Ok(None);
        };
        let Some(buff) = self.inflight.remove(&bus_addr) else {
            return Err(other_error("reclaimed unknown rx buffer"));
        };
        let packet_len = len.min(self.config.buf_size).min(buff.len());
        buff.complete_for_cpu(0, packet_len);
        Ok(Some((buff, packet_len)))
    }

    pub fn id(&self) -> usize {
        self.interface.id()
    }

    pub fn buf_size(&self) -> usize {
        self.config.buf_size
    }

    pub fn try_receive(&mut self) -> Option<RxPacket<'_>> {
        match self.reclaim_packet() {
            Ok(Some((buff, len))) => Some(RxPacket {
                queue: self,
                len,
                buff: Some(buff),
            }),
            Ok(None) | Err(_) => None,
        }
    }

    pub fn receive<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let packet = self.try_receive()?;
        Some(packet.consume(f))
    }
}

pub struct RxPacket<'a> {
    queue: &'a mut RxQueue,
    len: usize,
    buff: Option<ContiguousBuffer>,
}

impl RxPacket<'_> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn consume<R>(mut self, f: impl FnOnce(&[u8]) -> R) -> R {
        let buff = self.buff.as_ref().expect("rx packet buffer should exist");
        let ret = buff.read_with_cpu(self.len, f);
        if let Some(buff) = self.buff.take() {
            let _ = self.queue.submit_buffer(buff);
        }
        ret
    }
}

impl Drop for RxPacket<'_> {
    fn drop(&mut self) {
        if let Some(buff) = self.buff.take() {
            let _ = self.queue.submit_buffer(buff);
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::{
        any::Any,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
        task::{RawWaker, RawWakerVTable, Waker},
    };

    use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle};
    use rdif_eth::{DriverGeneric, Event, IRxQueue, ITxQueue, IdList, Interface};

    use super::*;

    struct TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<DmaAllocHandle> {
            panic!("test should not allocate contiguous DMA")
        }

        unsafe fn dealloc_contiguous(&self, _handle: DmaAllocHandle) {
            panic!("test should not deallocate contiguous DMA")
        }

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<DmaAllocHandle> {
            panic!("test should not allocate coherent DMA")
        }

        unsafe fn dealloc_coherent(&self, _handle: DmaAllocHandle) {
            panic!("test should not deallocate coherent DMA")
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            panic!("test should not map streaming DMA")
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {
            panic!("test should not unmap streaming DMA")
        }
    }

    struct TestInterface {
        irq_events: Event,
        handle_calls: Arc<AtomicUsize>,
        owned_irq_handler: Option<rdif_eth::BIrqHandler>,
    }

    impl DriverGeneric for TestInterface {
        fn name(&self) -> &str {
            "test-net"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl Interface for TestInterface {
        fn mac_address(&self) -> [u8; 6] {
            [0x02, 0, 0, 0, 0, 1]
        }

        fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
            panic!("test should not create TX queue")
        }

        fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
            panic!("test should not create RX queue")
        }

        fn enable_irq(&mut self) {}

        fn disable_irq(&mut self) {}

        fn is_irq_enabled(&self) -> bool {
            false
        }

        fn handle_irq(&mut self) -> Event {
            self.handle_calls.fetch_add(1, Ordering::AcqRel);
            self.irq_events
        }

        fn take_irq_handler(&mut self) -> Option<rdif_eth::BIrqHandler> {
            self.owned_irq_handler.take()
        }
    }

    struct OwnedTestIrqHandler {
        irq_events: Event,
        handle_calls: Arc<AtomicUsize>,
    }

    impl rdif_eth::IrqHandler for OwnedTestIrqHandler {
        fn handle_irq(&mut self) -> Event {
            self.handle_calls.fetch_add(1, Ordering::AcqRel);
            self.irq_events
        }
    }

    fn count_waker(counter: Arc<AtomicUsize>) -> Waker {
        unsafe fn clone(data: *const ()) -> RawWaker {
            let counter = unsafe { Arc::<AtomicUsize>::from_raw(data.cast()) };
            let cloned = Arc::clone(&counter);
            let _ = Arc::into_raw(counter);
            RawWaker::new(Arc::into_raw(cloned).cast(), &VTABLE)
        }

        unsafe fn wake(data: *const ()) {
            let counter = unsafe { Arc::<AtomicUsize>::from_raw(data.cast()) };
            counter.fetch_add(1, Ordering::AcqRel);
        }

        unsafe fn wake_by_ref(data: *const ()) {
            let counter = unsafe { Arc::<AtomicUsize>::from_raw(data.cast()) };
            counter.fetch_add(1, Ordering::AcqRel);
            let _ = Arc::into_raw(counter);
        }

        unsafe fn drop(data: *const ()) {
            let _ = unsafe { Arc::<AtomicUsize>::from_raw(data.cast()) };
        }

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
        let raw = RawWaker::new(Arc::into_raw(counter).cast(), &VTABLE);
        unsafe { Waker::from_raw(raw) }
    }

    #[test]
    fn irq_handler_fast_path_returns_events_without_waking_registered_wakers() {
        static DMA: TestDma = TestDma;
        let mut rx = IdList::none();
        rx.insert(3);
        let mut tx = IdList::none();
        tx.insert(5);
        let interface_calls = Arc::new(AtomicUsize::new(0));
        let irq_calls = Arc::new(AtomicUsize::new(0));
        let mut net = Net::new(
            TestInterface {
                irq_events: Event::none(),
                handle_calls: Arc::clone(&interface_calls),
                owned_irq_handler: Some(Box::new(OwnedTestIrqHandler {
                    irq_events: Event {
                        tx_queue: tx,
                        rx_queue: rx,
                    },
                    handle_calls: Arc::clone(&irq_calls),
                })),
            },
            &DMA,
        );
        let rx_wake_count = Arc::new(AtomicUsize::new(0));
        let tx_wake_count = Arc::new(AtomicUsize::new(0));
        net.inner
            .rx_wakers
            .register(3)
            .register(&count_waker(Arc::clone(&rx_wake_count)));
        net.inner
            .tx_wakers
            .register(5)
            .register(&count_waker(Arc::clone(&tx_wake_count)));

        let mut irq = net.take_irq_handler().unwrap();
        let events = irq.handle_irq();

        assert!(events.rx_queue.contains(3));
        assert!(events.tx_queue.contains(5));
        assert_eq!(irq_calls.load(Ordering::Acquire), 1);
        assert_eq!(interface_calls.load(Ordering::Acquire), 0);
        assert_eq!(rx_wake_count.load(Ordering::Acquire), 0);
        assert_eq!(tx_wake_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn irq_handler_requires_owned_endpoint() {
        static DMA: TestDma = TestDma;
        let handle_calls = Arc::new(AtomicUsize::new(0));
        let mut net = Net::new(
            TestInterface {
                irq_events: Event::none(),
                handle_calls,
                owned_irq_handler: None,
            },
            &DMA,
        );

        assert!(net.take_irq_handler().is_none());
    }

    #[test]
    fn irq_handler_uses_owned_endpoint() {
        static DMA: TestDma = TestDma;
        let mut rx = IdList::none();
        rx.insert(1);
        let mut tx = IdList::none();
        tx.insert(2);
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let owned_calls = Arc::new(AtomicUsize::new(0));
        let mut net = Net::new(
            TestInterface {
                irq_events: Event::none(),
                handle_calls: Arc::clone(&fallback_calls),
                owned_irq_handler: Some(Box::new(OwnedTestIrqHandler {
                    irq_events: Event {
                        tx_queue: tx,
                        rx_queue: rx,
                    },
                    handle_calls: Arc::clone(&owned_calls),
                })),
            },
            &DMA,
        );

        let mut irq = net.take_irq_handler().unwrap();
        let events = irq.handle_irq();

        assert!(events.rx_queue.contains(1));
        assert!(events.tx_queue.contains(2));
        assert_eq!(owned_calls.load(Ordering::Acquire), 1);
        assert_eq!(fallback_calls.load(Ordering::Acquire), 0);
    }
}
