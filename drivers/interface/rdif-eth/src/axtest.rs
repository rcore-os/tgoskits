use alloc::{boxed::Box, string::String};
use core::ptr::NonNull;

use axtest::prelude::*;
use dma_api::DmaError;

use crate::{
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

impl crate::DriverGeneric for MockNic {
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

    fn take_irq_handler(&mut self) -> Option<crate::BIrqHandler> {
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

#[axtest]
fn rdif_eth_error_mapping_and_plain_config_rules_hold() {
    ax_assert!(matches!(
        crate::io::ErrorKind::from(NetError::NotSupported),
        crate::io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(NetError::Retry),
        crate::io::ErrorKind::Interrupted
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(NetError::NoMemory),
        crate::io::ErrorKind::OutOfMemory
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(NetError::LinkDown),
        crate::io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(NetError::Other(Box::new(MockError))),
        crate::io::ErrorKind::Other(_)
    ));

    ax_assert!(matches!(
        NetError::from(DmaError::NoMemory),
        NetError::NoMemory
    ));
    ax_assert!(matches!(
        NetError::from(DmaError::ZeroSizedBuffer),
        NetError::Other(_)
    ));

    let config = QueueConfig {
        dma_mask: 0xff,
        align: 16,
        buf_size: 1500,
        ring_size: 32,
    };
    ax_assert_eq!(config.align, 16);
    ax_assert_eq!(config.buf_size, 1500);
}

#[axtest]
fn rdif_eth_id_lists_and_events_track_queue_bits() {
    let mut ids = IdList::none();
    ax_assert!(!ids.contains(4));
    ids.insert(4);
    ids.insert(7);
    ax_assert!(ids.contains(4));
    ax_assert_eq!(
        ids.iter().collect::<alloc::vec::Vec<_>>(),
        alloc::vec![4, 7]
    );
    ids.remove(4);
    ax_assert_eq!(ids.iter().collect::<alloc::vec::Vec<_>>(), alloc::vec![7]);

    let event = Event {
        tx_queue: ids,
        rx_queue: IdList::none(),
    };
    ax_assert!(event.tx_queue.contains(7));
    ax_assert!(!event.rx_queue.contains(7));
}

#[axtest]
fn rdif_eth_queues_reclaim_submitted_dma_buffers() {
    let mut byte = 0u8;
    let buffer = DmaBuffer {
        virt: NonNull::from(&mut byte),
        bus_addr: 0x1000,
        len: 128,
    };

    let mut tx = MockQueue::new(1);
    ax_assert_eq!(ITxQueue::id(&tx), 1);
    ax_assert_eq!(ITxQueue::config(&tx).ring_size, 128);
    ITxQueue::submit(&mut tx, buffer).unwrap();
    ax_assert_eq!(ITxQueue::reclaim(&mut tx), Some(0x1000));
    ax_assert_eq!(ITxQueue::reclaim(&mut tx), None);

    let mut rx = MockQueue::new(2);
    IRxQueue::submit(&mut rx, buffer).unwrap();
    ax_assert_eq!(IRxQueue::reclaim(&mut rx), Some((0x1000, 64)));
    ax_assert_eq!(IRxQueue::reclaim(&mut rx), None);
}

#[axtest]
fn rdif_eth_interface_and_wifi_control_delegate_expected_paths() {
    let mut nic = MockNic::new();
    ax_assert_eq!(nic.name(), "mock-eth");
    ax_assert_eq!(nic.mac_address(), [2, 0, 0, 0, 0, 1]);
    ax_assert!(!nic.is_irq_enabled());
    nic.enable_irq();
    ax_assert!(nic.is_irq_enabled());
    nic.disable_irq();
    ax_assert!(!nic.is_irq_enabled());

    let mut handler = nic.take_irq_handler().unwrap();
    let event = handler.handle_irq();
    ax_assert!(event.tx_queue.contains(1));
    ax_assert!(event.rx_queue.contains(2));

    let tx = nic.create_tx_queue().unwrap();
    ax_assert_eq!(tx.id(), 1);
    let rx = nic.create_rx_queue().unwrap();
    ax_assert_eq!(rx.id(), 2);

    let wifi = nic.wifi_control().unwrap();
    wifi.connect("ssid", "pass").unwrap();
    wifi.start_ap_open(b"ap", 6).unwrap();
    wifi.set_rx_wake(wake_marker);
    let policy = wifi.link_policy().unwrap();
    ax_assert_eq!(policy.ip, [192, 168, 7, 1]);
    ax_assert_eq!(policy.prefix_len, 24);
    ax_assert_eq!(policy.dhcp_server_client_ip, Some([192, 168, 7, 2]));

    let _name = String::from("keeps alloc linked");
}
