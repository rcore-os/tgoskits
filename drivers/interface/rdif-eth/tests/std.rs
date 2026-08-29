extern crate alloc;

use alloc::{
    alloc::{alloc_zeroed, dealloc},
    boxed::Box,
    string::String,
    vec,
};
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

use dma_api::{
    DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
    DmaDomainId, DmaError, DmaMapHandle, DmaOp,
};
use rdif_eth::{
    DmaBuffer, DriverGeneric, FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo,
    NetDeviceParts, NetError, NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult,
    NetIrqSnapshot, NetIrqSourceId, NetPollGroupId, NetPollGroupParts, NetPollIrqControl,
    NetQueueId, NetQueuePairParts, NetRearmResult, QueueConfig, RxCompletion, SubmitError,
    WifiControl, WifiControlProgress, WifiLinkPolicy, WifiOperation, WifiTransaction, Wpa2Pmk,
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

struct MockIrqHandler;

impl NetHardIrqHandler for MockIrqHandler {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        NetHardIrqResult::Schedule(NetIrqSnapshot::all_queue_work())
    }
}

struct MockIrqControl {
    armed: bool,
}

impl NetPollIrqControl for MockIrqControl {
    fn quiesce(&mut self) -> Result<(), NetError> {
        self.armed = false;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        self.armed = false;
        Ok(())
    }

    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        self.armed = true;
        Ok(NetRearmResult::Idle)
    }
}

struct MockWifi {
    connects: usize,
    active: bool,
}

impl WifiControl for MockWifi {
    fn start(
        &mut self,
        operation: &WifiOperation,
        _now_nanos: u64,
    ) -> Result<WifiControlProgress, NetError> {
        match operation {
            WifiOperation::Connect {
                ssid,
                pmk: Some(pmk),
                entropy: _,
            } if ssid == "ssid" && pmk.bytes() == &[1; 32] => {
                self.connects += 1;
                self.active = true;
                Ok(WifiControlProgress::WaitForInterruptUntil {
                    deadline_nanos: 1_000,
                })
            }
            WifiOperation::Disconnect => Ok(WifiControlProgress::Complete),
            WifiOperation::StartOpenAccessPoint { ssid, channel }
                if ssid == b"ap" && *channel == 6 =>
            {
                Ok(WifiControlProgress::Complete)
            }
            _ => Err(NetError::NotSupported),
        }
    }

    fn advance(&mut self, _now_nanos: u64) -> Result<WifiControlProgress, NetError> {
        if !self.active {
            return Err(NetError::InvalidParts);
        }
        self.active = false;
        Ok(WifiControlProgress::Complete)
    }

    fn cancel(&mut self) -> Result<(), NetError> {
        self.active = false;
        Ok(())
    }

    fn startup_transaction(&self) -> Option<WifiTransaction> {
        Some(WifiTransaction::open_access_point(
            b"ap".to_vec(),
            6,
            WifiLinkPolicy {
                ip: [192, 168, 7, 1],
                prefix_len: 24,
                dhcp_server_client_ip: Some([192, 168, 7, 2]),
            },
        ))
    }
}

struct MockNic;

impl DriverGeneric for MockNic {
    fn name(&self) -> &str {
        "mock-eth"
    }
}

impl NetDevice for MockNic {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        let mac = [2, 0, 0, 0, 0, 1];
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new(self.name(), mac),
            control: Box::new(FixedNetControl::new(mac)),
            wifi_control: Some(Box::new(MockWifi {
                connects: 0,
                active: false,
            })),
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(7),
                queues: NetQueuePairParts {
                    tx: Box::new(MockTxQueue::new()),
                    rx: Box::new(MockRxQueue::new()),
                },
                irq_control: Box::new(MockIrqControl { armed: false }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(3),
                    Box::new(MockIrqHandler),
                )],
            }],
        })
    }
}

#[test]
fn rdif_eth_error_mapping_and_plain_config_rules_hold() {
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
    assert_eq!(queue_config().align, 64);
    assert_eq!(queue_config().buf_size, 2048);
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
fn net_device_parts_expose_typed_group_queue_and_irq_ownership() {
    let parts = Box::new(MockNic).into_parts().unwrap();
    assert_eq!(parts.info.driver_name, "mock-eth");
    assert_eq!(parts.info.mac_address, [2, 0, 0, 0, 0, 1]);
    assert_eq!(parts.poll_groups.len(), 1);

    let mut group = parts.poll_groups.into_iter().next().unwrap();
    assert_eq!(group.id.get(), 7);
    assert_eq!(group.queues.tx.id().get(), 1);
    assert_eq!(group.queues.rx.id().get(), 2);
    assert_eq!(group.irq_endpoints[0].source_id().get(), 3);
    assert!(matches!(
        group.irq_endpoints[0].handle_irq(),
        NetHardIrqResult::Schedule(snapshot)
            if snapshot.contains(NetIrqSnapshot::RX) && snapshot.contains(NetIrqSnapshot::TX)
    ));
    group.irq_control.quiesce().unwrap();
    assert_eq!(
        group.irq_control.rearm_and_check(0).unwrap(),
        NetRearmResult::Idle
    );
}

#[test]
fn wifi_control_keeps_only_owned_control_operations() {
    let mut wifi = MockWifi {
        connects: 0,
        active: false,
    };
    let connect = WifiTransaction::connect_wpa2_pmk("ssid", Wpa2Pmk::new([1; 32]));
    assert_eq!(
        wifi.start(connect.operation(), 10).unwrap(),
        WifiControlProgress::WaitForInterruptUntil {
            deadline_nanos: 1_000,
        }
    );
    assert_eq!(wifi.advance(11).unwrap(), WifiControlProgress::Complete);
    let startup = wifi.startup_transaction().unwrap();
    assert_eq!(
        wifi.start(startup.operation(), 12).unwrap(),
        WifiControlProgress::Complete
    );
    let policy = startup.link_policy().unwrap();
    assert_eq!(policy.ip, [192, 168, 7, 1]);
    assert_eq!(policy.prefix_len, 24);
    assert_eq!(policy.dhcp_server_client_ip, Some([192, 168, 7, 2]));
    assert_eq!(wifi.connects, 1);

    let _name = String::from("keeps alloc linked");
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
