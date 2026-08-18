extern crate alloc;

use alloc::{boxed::Box, string::String};
use core::ptr::NonNull;

use dma_api::DmaError;
use rdif_eth::{
    DmaBuffer, DriverGeneric, Event, IRxQueue, ITxQueue, IdList, Interface, IrqHandler, NetError,
    QueueConfig, WifiControl, WifiLinkPolicy,
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

struct MockQueue {
    id: usize,
    last_bus_addr: Option<u64>,
    completed: Option<(u64, usize)>,
}

impl MockQueue {
    const fn new(id: usize) -> Self {
        Self {
            id,
            last_bus_addr: None,
            completed: None,
        }
    }

    const fn config() -> QueueConfig {
        QueueConfig {
            dma_mask: 0xffff_ffff,
            align: 64,
            buf_size: 2048,
            ring_size: 128,
        }
    }
}

impl ITxQueue for MockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn config(&self) -> QueueConfig {
        Self::config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.last_bus_addr = Some(buffer.bus_addr);
        self.completed = Some((buffer.bus_addr, buffer.len));
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.completed.take().map(|(bus_addr, _)| bus_addr)
    }
}

impl IRxQueue for MockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn config(&self) -> QueueConfig {
        Self::config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.last_bus_addr = Some(buffer.bus_addr);
        self.completed = Some((buffer.bus_addr, buffer.len / 2));
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        self.completed.take()
    }
}

struct MockIrqHandler;

impl IrqHandler for MockIrqHandler {
    fn handle_irq(&mut self) -> Event {
        let mut event = Event::none();
        event.tx_queue.insert(1);
        event.rx_queue.insert(2);
        event
    }
}

struct MockNic {
    irq_enabled: bool,
    wifi_connects: usize,
    wake: Option<fn()>,
}

impl MockNic {
    const fn new() -> Self {
        Self {
            irq_enabled: false,
            wifi_connects: 0,
            wake: None,
        }
    }
}

impl rdif_eth::DriverGeneric for MockNic {
    fn name(&self) -> &str {
        "mock-eth"
    }
}

impl Interface for MockNic {
    fn mac_address(&self) -> [u8; 6] {
        [2, 0, 0, 0, 0, 1]
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        Some(Box::new(MockQueue::new(1)))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        Some(Box::new(MockQueue::new(2)))
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        MockIrqHandler.handle_irq()
    }

    fn take_irq_handler(&mut self) -> Option<rdif_eth::BIrqHandler> {
        Some(Box::new(MockIrqHandler))
    }

    fn wifi_control(&mut self) -> Option<&mut dyn WifiControl> {
        Some(self)
    }
}

impl WifiControl for MockNic {
    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), NetError> {
        if ssid != "ssid" || password != "pass" {
            return Err(NetError::NotSupported);
        }
        self.wifi_connects += 1;
        Ok(())
    }

    fn disconnect(&mut self) -> Result<(), NetError> {
        Ok(())
    }

    fn start_ap_open(&mut self, ssid: &[u8], channel: u8) -> Result<(), NetError> {
        if ssid != b"ap" || channel != 6 {
            return Err(NetError::NotSupported);
        }
        Ok(())
    }

    fn set_rx_wake(&mut self, wake: fn()) {
        self.wake = Some(wake);
    }

    fn link_policy(&self) -> Option<WifiLinkPolicy> {
        Some(WifiLinkPolicy {
            ip: [192, 168, 7, 1],
            prefix_len: 24,
            dhcp_server_client_ip: Some([192, 168, 7, 2]),
        })
    }
}

fn wake_marker() {}

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

    let config = QueueConfig {
        dma_mask: 0xff,
        align: 16,
        buf_size: 1500,
        ring_size: 32,
    };
    assert_eq!(config.align, 16);
    assert_eq!(config.buf_size, 1500);
}

#[test]
fn rdif_eth_id_lists_and_events_track_queue_bits() {
    let mut ids = IdList::none();
    assert!(!ids.contains(4));
    ids.insert(4);
    ids.insert(7);
    assert!(ids.contains(4));
    assert_eq!(
        ids.iter().collect::<alloc::vec::Vec<_>>(),
        alloc::vec![4, 7]
    );
    ids.remove(4);
    assert_eq!(ids.iter().collect::<alloc::vec::Vec<_>>(), alloc::vec![7]);

    let event = Event {
        tx_queue: ids,
        rx_queue: IdList::none(),
    };
    assert!(event.tx_queue.contains(7));
    assert!(!event.rx_queue.contains(7));
}

#[test]
fn rdif_eth_queues_reclaim_submitted_dma_buffers() {
    let mut byte = 0u8;
    let buffer = DmaBuffer {
        virt: NonNull::from(&mut byte),
        bus_addr: 0x1000,
        len: 128,
    };

    let mut tx = MockQueue::new(1);
    assert_eq!(ITxQueue::id(&tx), 1);
    assert_eq!(ITxQueue::config(&tx).ring_size, 128);
    ITxQueue::submit(&mut tx, buffer).unwrap();
    assert_eq!(ITxQueue::reclaim(&mut tx), Some(0x1000));
    assert_eq!(ITxQueue::reclaim(&mut tx), None);

    let mut rx = MockQueue::new(2);
    IRxQueue::submit(&mut rx, buffer).unwrap();
    assert_eq!(IRxQueue::reclaim(&mut rx), Some((0x1000, 64)));
    assert_eq!(IRxQueue::reclaim(&mut rx), None);
}

#[test]
fn rdif_eth_interface_and_wifi_control_delegate_expected_paths() {
    let mut nic = MockNic::new();
    assert_eq!(nic.name(), "mock-eth");
    assert_eq!(nic.mac_address(), [2, 0, 0, 0, 0, 1]);
    assert!(!nic.is_irq_enabled());
    nic.enable_irq();
    assert!(nic.is_irq_enabled());
    nic.disable_irq();
    assert!(!nic.is_irq_enabled());

    let mut handler = nic.take_irq_handler().unwrap();
    let event = handler.handle_irq();
    assert!(event.tx_queue.contains(1));
    assert!(event.rx_queue.contains(2));

    let tx = nic.create_tx_queue().unwrap();
    assert_eq!(tx.id(), 1);
    let rx = nic.create_rx_queue().unwrap();
    assert_eq!(rx.id(), 2);

    let wifi = nic.wifi_control().unwrap();
    wifi.connect("ssid", "pass").unwrap();
    wifi.start_ap_open(b"ap", 6).unwrap();
    wifi.set_rx_wake(wake_marker);
    let policy = wifi.link_policy().unwrap();
    assert_eq!(policy.ip, [192, 168, 7, 1]);
    assert_eq!(policy.prefix_len, 24);
    assert_eq!(policy.dhcp_server_client_ip, Some([192, 168, 7, 2]));

    let _name = String::from("keeps alloc linked");
}
