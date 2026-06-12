use alloc::{boxed::Box, collections::VecDeque, string::String, vec::Vec};

use ax_sync::spin::SpinNoIrq;
use rd_net::{Net, NetError, RxQueue, TxQueue};

const RX_PREFETCH_TARGET: usize = 1;
const ETH_ZLEN: usize = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetDeviceError {
    Again,
    BadState,
    InvalidParam,
    Io,
    NoMemory,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetIrqEvents(u32);

impl NetIrqEvents {
    pub const RX_READY: Self = Self(1 << 0);
    pub const TX_DONE: Self = Self(1 << 1);
    pub const RX_ERROR: Self = Self(1 << 2);
    pub const SPURIOUS: Self = Self(1 << 31);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl core::ops::BitOr for NetIrqEvents {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for NetIrqEvents {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

pub type NetDeviceResult<T = ()> = Result<T, NetDeviceError>;

pub trait NetRxBuffer: Send {
    fn packet(&self) -> &[u8];
    fn packet_len(&self) -> usize {
        self.packet().len()
    }
}

pub trait NetTxBuffer: Send {
    fn packet(&self) -> &[u8];
    fn packet_mut(&mut self) -> &mut [u8];
    fn packet_len(&self) -> usize;
}

pub trait EthernetDriver: Send + Sync {
    fn device_name(&self) -> &str;
    fn irq_num(&self) -> Option<usize>;
    fn enable_irq(&mut self);
    fn disable_irq(&mut self);
    fn mac_address(&self) -> [u8; 6];
    fn alloc_tx_buffer(&mut self, size: usize) -> NetDeviceResult<Box<dyn NetTxBuffer>>;
    fn recycle_tx_buffers(&mut self) -> NetDeviceResult;
    fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult;
    fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>>;
    fn recycle_rx_buffer(&mut self, rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult;
    fn handle_irq(&mut self) -> NetIrqEvents;
}

pub type EthernetDeviceList = Vec<Box<dyn EthernetDriver>>;

struct VecTxBuffer {
    packet: Vec<u8>,
}

impl VecTxBuffer {
    fn new(size: usize) -> Self {
        Self {
            packet: alloc::vec![0; size],
        }
    }
}

impl NetTxBuffer for VecTxBuffer {
    fn packet(&self) -> &[u8] {
        &self.packet
    }

    fn packet_mut(&mut self) -> &mut [u8] {
        &mut self.packet
    }

    fn packet_len(&self) -> usize {
        self.packet.len()
    }
}

struct VecRxBuffer {
    packet: Vec<u8>,
}

impl NetRxBuffer for VecRxBuffer {
    fn packet(&self) -> &[u8] {
        &self.packet
    }
}

struct RdNetState {
    tx_queue: TxQueue,
    rx_queue: RxQueue,
    pending_rx: VecDeque<VecRxBuffer>,
}

pub struct RdNetDriver {
    name: String,
    mac: [u8; 6],
    irq: Option<usize>,
    irq_handler: Option<rd_net::IrqHandler>,
    state: SpinNoIrq<RdNetState>,
}

impl RdNetDriver {
    pub fn new(name: impl Into<String>, mut net: Net, irq: Option<usize>) -> NetDeviceResult<Self> {
        let mac = net.mac_address();
        let tx_queue = net.create_tx_queue().map_err(map_net_error)?;
        let rx_queue = net.create_rx_queue().map_err(map_net_error)?;
        let irq_handler = irq.as_ref().map(|_| net.irq_handler());

        Ok(Self {
            name: name.into(),
            mac,
            irq,
            irq_handler,
            state: SpinNoIrq::new(RdNetState {
                tx_queue,
                rx_queue,
                pending_rx: VecDeque::with_capacity(RX_PREFETCH_TARGET),
            }),
        })
    }

    fn prefetch_rx_packets(&self, state: &mut RdNetState, target: usize) -> NetDeviceResult {
        while state.pending_rx.len() < target {
            let Some(packet) = state.rx_queue.receive(|packet| VecRxBuffer {
                packet: packet.to_vec(),
            }) else {
                break;
            };
            state.pending_rx.push_back(packet);
        }
        Ok(())
    }
}

impl EthernetDriver for RdNetDriver {
    fn device_name(&self) -> &str {
        &self.name
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq
    }

    fn enable_irq(&mut self) {
        if let Some(handler) = &self.irq_handler {
            handler.enable();
        }
    }

    fn disable_irq(&mut self) {
        if let Some(handler) = &self.irq_handler {
            handler.disable();
        }
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> NetDeviceResult<Box<dyn NetTxBuffer>> {
        let capacity = self.state.lock().tx_queue.buf_size();
        if size > capacity {
            return Err(NetDeviceError::InvalidParam);
        }
        Ok(Box::new(VecTxBuffer::new(size)))
    }

    fn recycle_tx_buffers(&mut self) -> NetDeviceResult {
        Ok(())
    }

    fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult {
        let packet_len = tx_buf.packet_len();
        let tx_len = packet_len.max(ETH_ZLEN);
        let mut state = self.state.lock();
        let (_ret, mut pending) = state
            .tx_queue
            .prepare_send(tx_len, |buffer| {
                let packet = tx_buf.packet_mut();
                buffer[..packet_len].copy_from_slice(packet);
                buffer[packet_len..tx_len].fill(0);
            })
            .map_err(map_net_error)?;
        pending.try_submit().map_err(map_net_error)
    }

    fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>> {
        let mut state = self.state.lock();
        self.prefetch_rx_packets(&mut state, RX_PREFETCH_TARGET)?;
        state
            .pending_rx
            .pop_front()
            .map(|packet| Box::new(packet) as Box<dyn NetRxBuffer>)
            .ok_or(NetDeviceError::Again)
    }

    fn recycle_rx_buffer(&mut self, _rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult {
        Ok(())
    }

    fn handle_irq(&mut self) -> NetIrqEvents {
        let Some(handler) = &self.irq_handler else {
            return NetIrqEvents::SPURIOUS;
        };

        handler.handle();

        let mut events = NetIrqEvents::empty();
        let mut state = self.state.lock();
        if let Err(err) = self.prefetch_rx_packets(&mut state, RX_PREFETCH_TARGET) {
            warn!(
                "failed to prefetch rx packets for {} during irq: {err:?}",
                self.name
            );
            events |= NetIrqEvents::RX_ERROR;
        }
        if !state.pending_rx.is_empty() {
            events |= NetIrqEvents::RX_READY;
        }

        if events.is_empty() {
            NetIrqEvents::SPURIOUS
        } else {
            events
        }
    }
}

fn map_net_error(err: NetError) -> NetDeviceError {
    match err {
        NetError::Retry => NetDeviceError::Again,
        NetError::NoMemory => NetDeviceError::NoMemory,
        NetError::NotSupported => NetDeviceError::Unsupported,
        NetError::LinkDown | NetError::Other(_) => NetDeviceError::Io,
    }
}
