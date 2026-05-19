extern crate alloc;

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};

use ax_driver_base::DevError;
use ax_driver_net::{NetDriverOps, NetIrqEvent};
use rd_net::{DmaBuffer, Event, IRxQueue, ITxQueue, NetError, QueueConfig};
use rdrive::{DriverGeneric, PlatformDevice};
use spin::Mutex;

#[cfg(feature = "fxmac")]
pub mod fxmac;
#[cfg(feature = "ixgbe")]
pub mod ixgbe;

const BUFFER_SIZE: usize = 2048;

pub fn register_net<D>(plat_dev: PlatformDevice, driver: D)
where
    D: NetDriverOps + 'static,
{
    let net = rd_net::Net::new(LegacyNetDevice::new(driver), axklib::dma::op());
    plat_dev.register(net);
}

struct LegacyNetDevice<D> {
    inner: Arc<Mutex<D>>,
    irq_enabled: bool,
    tx_created: bool,
    rx_created: bool,
}

impl<D> LegacyNetDevice<D> {
    fn new(driver: D) -> Self {
        Self {
            inner: Arc::new(Mutex::new(driver)),
            irq_enabled: false,
            tx_created: false,
            rx_created: false,
        }
    }
}

impl<D: NetDriverOps + 'static> DriverGeneric for LegacyNetDevice<D> {
    fn name(&self) -> &str {
        "legacy-net"
    }
}

impl<D: NetDriverOps + 'static> rd_net::Interface for LegacyNetDevice<D> {
    fn mac_address(&self) -> [u8; 6] {
        self.inner.lock().mac_address().0
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        self.tx_created = true;
        Some(Box::new(LegacyTxQueue {
            inner: Arc::clone(&self.inner),
            completed: VecDeque::new(),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        self.rx_created = true;
        Some(Box::new(LegacyRxQueue {
            inner: Arc::clone(&self.inner),
            submitted: VecDeque::new(),
            completed: VecDeque::new(),
        }))
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
        let event = self.inner.lock().handle_irq();
        let mut out = Event::none();
        if event.contains(NetIrqEvent::TX_DONE) {
            out.tx_queue.insert(0);
        }
        if event.contains(NetIrqEvent::RX_READY) {
            out.rx_queue.insert(0);
        }
        out
    }
}

struct LegacyTxQueue<D> {
    inner: Arc<Mutex<D>>,
    completed: VecDeque<u64>,
}

impl<D: NetDriverOps + 'static> ITxQueue for LegacyTxQueue<D> {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        let driver = self.inner.lock();
        QueueConfig {
            dma_mask: u64::MAX,
            align: 1,
            buf_size: BUFFER_SIZE,
            ring_size: driver.tx_queue_size().max(2),
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        let packet = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        let mut driver = self.inner.lock();
        let mut tx = driver
            .alloc_tx_buffer(packet.len())
            .map_err(map_net_error)?;
        tx.packet_mut().copy_from_slice(packet);
        driver.transmit(tx).map_err(map_net_error)?;
        self.completed.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        let _ = self.inner.lock().recycle_tx_buffers();
        self.completed.pop_front()
    }
}

struct LegacyRxQueue<D> {
    inner: Arc<Mutex<D>>,
    submitted: VecDeque<SubmittedRxBuffer>,
    completed: VecDeque<(u64, usize)>,
}

struct SubmittedRxBuffer {
    virt_addr: usize,
    bus_addr: u64,
    len: usize,
}

impl<D: NetDriverOps + 'static> LegacyRxQueue<D> {
    fn poll_receive(&mut self) {
        loop {
            let Some(buffer) = self.submitted.pop_front() else {
                return;
            };

            let mut driver = self.inner.lock();
            match driver.receive() {
                Ok(packet) => {
                    let len = packet.packet_len().min(buffer.len);
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            packet.packet().as_ptr(),
                            buffer.virt_addr as *mut u8,
                            len,
                        );
                    }
                    let _ = driver.recycle_rx_buffer(packet);
                    self.completed.push_back((buffer.bus_addr, len));
                }
                Err(DevError::Again) => {
                    self.submitted.push_front(buffer);
                    return;
                }
                Err(_) => return,
            }
        }
    }
}

impl<D: NetDriverOps + 'static> IRxQueue for LegacyRxQueue<D> {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        let driver = self.inner.lock();
        QueueConfig {
            dma_mask: u64::MAX,
            align: 1,
            buf_size: BUFFER_SIZE,
            ring_size: driver.rx_queue_size().max(2),
        }
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.submitted.push_back(SubmittedRxBuffer {
            virt_addr: buffer.virt.as_ptr() as usize,
            bus_addr: buffer.bus_addr,
            len: buffer.len,
        });
        self.poll_receive();
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        self.poll_receive();
        self.completed.pop_front()
    }
}

fn map_net_error(err: DevError) -> NetError {
    match err {
        DevError::Again | DevError::ResourceBusy => NetError::Retry,
        DevError::NoMemory => NetError::NoMemory,
        DevError::Unsupported => NetError::NotSupported,
        _ => NetError::Other(Box::new(rd_net::KError::Unknown("legacy net error"))),
    }
}
