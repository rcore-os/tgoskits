extern crate alloc;

use alloc::{boxed::Box, collections::VecDeque, sync::Arc};

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use ax_driver_net::{
    EthernetAddress, NetBuf, NetBufBox, NetBufPool, NetBufPtr, NetDriverOps, NetIrqEvent,
};
use ax_kspin::SpinNoIrq;
use rd_net::{Interface, NetError};
use rdrive::{Device, DriverGeneric};

use super::DmaImpl;

#[cfg(feature = "intel-net")]
mod intel;
#[cfg(feature = "virtio-net-pci")]
mod virtio_pci;

const NET_BUF_LEN: usize = 2048;
const NET_BUF_POOL_CAPACITY: usize = 512;
const NET_QUEUE_SIZE: usize = 256;
const RX_PREFETCH_TARGET: usize = 1;

pub struct PlatformNetDevice {
    name: &'static str,
    net: rd_net::Net,
    irq_num: Option<usize>,
}

impl PlatformNetDevice {
    fn new(name: &'static str, net: rd_net::Net, irq_num: Option<usize>) -> Self {
        Self { name, net, irq_num }
    }
}

impl DriverGeneric for PlatformNetDevice {
    fn name(&self) -> &str {
        self.name
    }
}

pub struct PlatformNetDriver {
    name: &'static str,
    dev: Option<Box<dyn NetDriverOps>>,
}

impl DriverGeneric for PlatformNetDriver {
    fn name(&self) -> &str {
        self.name
    }
}

struct NetState {
    tx_queue: rd_net::TxQueue,
    rx_queue: rd_net::RxQueue,
    pending_rx: VecDeque<NetBufBox>,
}

pub struct Net {
    name: &'static str,
    mac: [u8; 6],
    irq_num: Option<usize>,
    buf_pool: Arc<NetBufPool>,
    irq_handler: Option<rd_net::IrqHandler>,
    state: SpinNoIrq<NetState>,
}

impl TryFrom<Device<PlatformNetDevice>> for Net {
    type Error = DevError;

    fn try_from(device: Device<PlatformNetDevice>) -> Result<Self, Self::Error> {
        let mut dev = device.lock().map_err(map_device_err_to_dev_err)?;
        let name = dev.name;
        let mac = dev.net.mac_address();
        let irq_num = dev.irq_num;
        let tx_queue = dev.net.create_tx_queue().map_err(map_net_err_to_dev_err)?;
        let rx_queue = dev.net.create_rx_queue().map_err(map_net_err_to_dev_err)?;
        let irq_handler = irq_num.map(|_| dev.net.irq_handler());
        drop(dev);

        Ok(Self {
            name,
            mac,
            irq_num,
            buf_pool: NetBufPool::new(NET_BUF_POOL_CAPACITY, NET_BUF_LEN)?,
            irq_handler,
            state: SpinNoIrq::new(NetState {
                tx_queue,
                rx_queue,
                pending_rx: VecDeque::with_capacity(RX_PREFETCH_TARGET),
            }),
        })
    }
}

impl Net {
    fn prefetch_rx_packets(&self, state: &mut NetState, target: usize) -> DevResult {
        while state.pending_rx.len() < target {
            let Some(result) = state.rx_queue.receive(|packet| {
                let Some(mut net_buf) = self.buf_pool.alloc_boxed() else {
                    return Err(DevError::NoMemory);
                };

                if packet.len() > net_buf.capacity() {
                    warn!(
                        "dropping oversized rx packet for {}: {} bytes",
                        self.name,
                        packet.len()
                    );
                    return Err(DevError::InvalidParam);
                }

                net_buf.set_header_len(0);
                net_buf.set_packet_len(packet.len());
                net_buf.packet_mut().copy_from_slice(packet);
                Ok(net_buf)
            }) else {
                break;
            };

            match result {
                Ok(net_buf) => state.pending_rx.push_back(net_buf),
                Err(DevError::InvalidParam) => continue,
                Err(err) => return Err(err),
            }
        }

        Ok(())
    }
}

pub(super) fn pci_legacy_irq_for_address(address: rdrive::probe::pci::PciAddress) -> usize {
    const PCI_IRQ_BASE: usize = if cfg!(target_arch = "x86_64") || cfg!(target_arch = "riscv64") {
        0x20
    } else if cfg!(target_arch = "loongarch64") {
        0x10
    } else if cfg!(target_arch = "aarch64") {
        0x23
    } else {
        0
    };

    PCI_IRQ_BASE + (usize::from(address.device()) & 3)
}

impl BaseDriverOps for Net {
    fn device_name(&self) -> &str {
        self.name
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Net
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }
}

impl NetDriverOps for Net {
    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.mac)
    }

    fn can_transmit(&self) -> bool {
        let mut state = self.state.lock();
        match state.tx_queue.prepare_send(0, |_| ()) {
            Ok((_ret, _pending)) => true,
            Err(NetError::Retry) => false,
            Err(err) => {
                warn!("failed to test tx readiness for {}: {err:?}", self.name);
                false
            }
        }
    }

    fn can_receive(&self) -> bool {
        let mut state = self.state.lock();
        if let Err(err) = self.prefetch_rx_packets(&mut state, RX_PREFETCH_TARGET) {
            warn!("failed to prefetch rx packets for {}: {err:?}", self.name);
        }
        !state.pending_rx.is_empty()
    }

    fn rx_queue_size(&self) -> usize {
        NET_QUEUE_SIZE
    }

    fn tx_queue_size(&self) -> usize {
        NET_QUEUE_SIZE
    }

    fn recycle_rx_buffer(&mut self, rx_buf: NetBufPtr) -> DevResult {
        drop(unsafe { NetBuf::from_buf_ptr(rx_buf) });
        Ok(())
    }

    fn recycle_tx_buffers(&mut self) -> DevResult {
        Ok(())
    }

    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult {
        let tx_buf = unsafe { NetBuf::from_buf_ptr(tx_buf) };
        let packet = tx_buf.packet();

        let mut state = self.state.lock();
        let (_ret, mut pending) = state
            .tx_queue
            .prepare_send(packet.len(), |buffer| {
                buffer[..packet.len()].copy_from_slice(packet);
            })
            .map_err(map_net_err_to_dev_err)?;
        pending.try_submit().map_err(map_net_err_to_dev_err)?;
        drop(tx_buf);
        Ok(())
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        let mut state = self.state.lock();
        self.prefetch_rx_packets(&mut state, RX_PREFETCH_TARGET)?;
        state
            .pending_rx
            .pop_front()
            .map(NetBuf::into_buf_ptr)
            .ok_or(DevError::Again)
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        let mut net_buf = self.buf_pool.alloc_boxed().ok_or(DevError::NoMemory)?;
        if size > net_buf.capacity() {
            return Err(DevError::InvalidParam);
        }

        net_buf.set_header_len(0);
        net_buf.set_packet_len(size);
        Ok(net_buf.into_buf_ptr())
    }

    fn handle_irq(&mut self) -> NetIrqEvent {
        let Some(handler) = &self.irq_handler else {
            return NetIrqEvent::SPURIOUS;
        };

        handler.handle();

        let mut events = NetIrqEvent::empty();
        let mut state = self.state.lock();
        if let Err(err) = self.prefetch_rx_packets(&mut state, RX_PREFETCH_TARGET) {
            warn!(
                "failed to prefetch rx packets for {} during irq: {err:?}",
                self.name
            );
            events |= NetIrqEvent::RX_ERROR;
        }
        if !state.pending_rx.is_empty() {
            events |= NetIrqEvent::RX_READY;
        }

        if events.is_empty() {
            NetIrqEvent::SPURIOUS
        } else {
            events
        }
    }
}

pub trait PlatformDeviceNet {
    fn register_net<T>(self, name: &'static str, dev: T, irq_num: Option<usize>)
    where
        T: Interface + 'static;
}

impl PlatformDeviceNet for rdrive::PlatformDevice {
    fn register_net<T>(self, name: &'static str, dev: T, irq_num: Option<usize>)
    where
        T: Interface + 'static,
    {
        let net = rd_net::Net::new(dev, &DmaImpl);
        self.register(PlatformNetDevice::new(name, net, irq_num));
    }
}

pub trait PlatformDeviceNetDriver {
    fn register_net_driver<T>(self, name: &'static str, dev: T)
    where
        T: NetDriverOps + 'static;
}

impl PlatformDeviceNetDriver for rdrive::PlatformDevice {
    fn register_net_driver<T>(self, name: &'static str, dev: T)
    where
        T: NetDriverOps + 'static,
    {
        self.register(PlatformNetDriver {
            name,
            dev: Some(Box::new(dev)),
        });
    }
}

pub(super) fn take_net_driver(
    device: Device<PlatformNetDriver>,
) -> Result<Box<dyn NetDriverOps>, DevError> {
    let mut dev = device.lock().map_err(map_device_err_to_dev_err)?;
    dev.dev.take().ok_or(DevError::BadState)
}

fn map_net_err_to_dev_err(err: NetError) -> DevError {
    match err {
        NetError::Retry => DevError::Again,
        NetError::NoMemory => DevError::NoMemory,
        NetError::NotSupported => DevError::Unsupported,
        NetError::LinkDown | NetError::Other(_) => DevError::Io,
    }
}

fn map_device_err_to_dev_err(_err: rdrive::GetDeviceError) -> DevError {
    DevError::BadState
}
