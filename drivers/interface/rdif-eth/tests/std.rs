extern crate alloc;
// Link the runtime-owned synchronization provider used by DMA pool tests.
extern crate ax_runtime as _;

use alloc::{
    alloc::{alloc_zeroed, dealloc},
    boxed::Box,
};
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
    DmaDomainId, DmaError, DmaMapHandle, DmaOp,
};
use rdif_eth::{
    DmaBuffer, IRxQueue, ITxQueue, NetError, NetQueueId, QueueConfig, RxCompletion, SubmitError,
    WifiOperation, WifiTransaction, Wpa2Pmk,
};

struct MockError;

impl core::fmt::Debug for MockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MockError")
    }
}

impl core::fmt::Display for MockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("mock error")
    }
}

impl core::error::Error for MockError {}

struct TestDma;

impl TestDma {
    unsafe fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe {
            DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
        })
    }
}

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::allocate(layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::allocate(layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(
            unsafe {
                DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
            },
        )
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

static TEST_DMA: TestDma = TestDma;

fn dma_buffer(len: usize) -> DmaBuffer {
    let dev = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let pool = dev.contiguous_buffer_pool(
        Layout::from_size_align(256, 64).unwrap(),
        DmaDirection::Bidirectional,
        1,
    );
    match DmaBuffer::new(pool.alloc().unwrap(), len) {
        Ok(buffer) => buffer,
        Err(_) => panic!("test DMA token length must fit its allocation"),
    }
}

struct MockTxQueue {
    completed: Option<DmaBuffer>,
    reject_next: bool,
}

impl MockTxQueue {
    const fn new() -> Self {
        Self {
            completed: None,
            reject_next: false,
        }
    }
}

impl ITxQueue for MockTxQueue {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(1)
    }

    fn config(&self) -> QueueConfig {
        queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if core::mem::take(&mut self.reject_next) {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        self.completed = Some(buffer);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        self.completed.take()
    }
}

struct MockRxQueue {
    completed: Option<DmaBuffer>,
}

impl MockRxQueue {
    const fn new() -> Self {
        Self { completed: None }
    }
}

impl IRxQueue for MockRxQueue {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(2)
    }

    fn config(&self) -> QueueConfig {
        queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        self.completed = Some(buffer);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<RxCompletion> {
        self.completed.take().map(|buffer| RxCompletion {
            packet_len: buffer.len() / 2,
            buffer,
        })
    }
}

const fn queue_config() -> QueueConfig {
    QueueConfig {
        dma_mask: u64::MAX,
        align: 64,
        buf_size: 2048,
        ring_size: 128,
    }
}

#[test]
fn rdif_eth_errors_map_to_io_kinds() {
    assert!(matches!(
        rdif_eth::io::ErrorKind::from(NetError::DeviceNotPresent),
        rdif_eth::io::ErrorKind::NotAvailable
    ));
    assert!(matches!(
        rdif_eth::io::ErrorKind::from(NetError::NotSupported),
        rdif_eth::io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        rdif_eth::io::ErrorKind::from(NetError::Retry),
        rdif_eth::io::ErrorKind::Interrupted
    ));
    assert!(matches!(
        rdif_eth::io::ErrorKind::from(NetError::NoMemory),
        rdif_eth::io::ErrorKind::OutOfMemory
    ));
    assert!(matches!(
        rdif_eth::io::ErrorKind::from(NetError::LinkDown),
        rdif_eth::io::ErrorKind::NotAvailable
    ));
    assert!(matches!(
        rdif_eth::io::ErrorKind::from(NetError::Other(Box::new(MockError))),
        rdif_eth::io::ErrorKind::Other(_)
    ));
    assert!(matches!(
        NetError::from(DmaError::NoMemory),
        NetError::NoMemory
    ));
    assert!(matches!(
        NetError::from(DmaError::ZeroSizedBuffer),
        NetError::Other(_)
    ));
}

#[test]
fn submit_failure_and_reclaim_preserve_unique_dma_token() {
    let mut tx = MockTxQueue::new();
    tx.reject_next = true;
    let buffer = dma_buffer(128);
    let bus_addr = buffer.bus_addr();
    let error = tx.submit(buffer).unwrap_err();
    assert!(matches!(error.error(), NetError::Retry));
    let buffer = error.into_buffer();
    assert_eq!(buffer.bus_addr(), bus_addr);
    tx.submit(buffer).unwrap();
    let reclaimed = tx.reclaim().unwrap();
    assert_eq!(reclaimed.bus_addr(), bus_addr);

    let mut rx = MockRxQueue::new();
    rx.submit(reclaimed).unwrap();
    let completion = rx.reclaim().unwrap();
    assert_eq!(completion.buffer.bus_addr(), bus_addr);
    assert_eq!(completion.packet_len, 64);
}

#[test]
fn wifi_transaction_only_fills_missing_secured_entropy() {
    let mut ordinary = WifiTransaction::connect_wpa2_pmk("ssid", Wpa2Pmk::new([1; 32]));
    assert!(ordinary.needs_connect_entropy());
    ordinary.provide_connect_entropy([7; 32]);
    assert!(!ordinary.needs_connect_entropy());
    assert!(matches!(
        ordinary.operation(),
        WifiOperation::Connect {
            entropy: Some(value),
            ..
        } if *value == [7; 32]
    ));

    let mut explicit =
        WifiTransaction::connect_wpa2_pmk_with_entropy("ssid", Wpa2Pmk::new([1; 32]), [3; 32]);
    explicit.provide_connect_entropy([9; 32]);
    assert!(matches!(
        explicit.operation(),
        WifiOperation::Connect {
            entropy: Some(value),
            ..
        } if *value == [3; 32]
    ));

    let mut open = WifiTransaction::connect_open("ssid");
    open.provide_connect_entropy([5; 32]);
    assert!(matches!(
        open.operation(),
        WifiOperation::Connect { entropy: None, .. }
    ));
}
