extern crate alloc;

use alloc::{boxed::Box, vec, vec::Vec};
use core::{num::NonZeroUsize, ptr::NonNull, time::Duration};

use dma_api::{DmaDirection, DmaMapHandle, DmaOp};
use rdif_eth::{Buffer, IRxQueue, ITxQueue, Interface, NetError};
use smoltcp::{
    iface::{Config, Interface as SmolInterface, SocketSet},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::icmp::{self, Socket as IcmpSocket},
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};

const LOCAL_IP: IpAddress = IpAddress::v4(10, 0, 2, 15);
const GATEWAY_IP: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);

fn now() -> Instant {
    let ms = crate::os::time::since_boot().as_millis() as i64;
    Instant::from_millis(ms)
}

fn spin_delay(duration: Duration) {
    let start = crate::os::time::since_boot();
    while crate::os::time::since_boot().saturating_sub(start) < duration {
        core::hint::spin_loop();
    }
}

struct RxSlot {
    storage: Vec<u8>,
    map: DmaMapHandle,
    req_id: Option<rdif_eth::RequestId>,
}

struct NetDevice {
    tx: Box<dyn ITxQueue>,
    rx: Box<dyn IRxQueue>,
    slots: Vec<RxSlot>,
}

impl NetDevice {
    fn new(tx: Box<dyn ITxQueue>, rx: Box<dyn IRxQueue>) -> Self {
        let cfg = rx.buff_config();
        let mut slots = Vec::new();

        for _ in 0..64 {
            let mut storage = vec![0u8; cfg.size.max(1536)];
            let map = unsafe {
                crate::os::mem::dma::kernel_dma_op()
                    .map_single(
                        cfg.dma_mask,
                        NonNull::new(storage.as_mut_ptr()).expect("nonnull rx buffer"),
                        NonZeroUsize::new(storage.len()).expect("nonzero rx buffer"),
                        cfg.align.max(1),
                        DmaDirection::FromDevice,
                    )
                    .expect("map rx buffer")
            };

            slots.push(RxSlot {
                storage,
                map,
                req_id: None,
            });
        }

        let mut dev = Self { tx, rx, slots };
        for idx in 0..dev.slots.len() {
            let _ = dev.refill_slot(idx);
        }
        dev
    }

    fn refill_slot(&mut self, idx: usize) -> core::result::Result<(), NetError> {
        let slot = &mut self.slots[idx];
        if slot.req_id.is_some() {
            return Ok(());
        }

        let req_id = self.rx.submit_request(rdif_eth::RxRequest {
            buffer: Buffer {
                virt: slot.storage.as_mut_ptr(),
                bus: slot.map.dma_addr().as_u64(),
                size: slot.storage.len(),
            },
        })?;

        slot.req_id = Some(req_id);
        Ok(())
    }

    fn poll_rx_packet(&mut self) -> Option<Vec<u8>> {
        for idx in 0..self.slots.len() {
            let req_id = match self.slots[idx].req_id {
                Some(id) => id,
                None => continue,
            };

            match self.rx.poll_request(req_id) {
                Ok(resp) => {
                    let len = resp.len.min(self.slots[idx].storage.len());
                    crate::os::mem::dma::kernel_dma_op().prepare_read(
                        &self.slots[idx].map,
                        0,
                        len,
                        DmaDirection::FromDevice,
                    );
                    let packet = self.slots[idx].storage[..len].to_vec();
                    self.slots[idx].req_id = None;
                    let _ = self.refill_slot(idx);
                    return Some(packet);
                }
                Err(NetError::Retry) => {}
                Err(_) => {
                    self.slots[idx].req_id = None;
                    let _ = self.refill_slot(idx);
                }
            }
        }

        None
    }
}

impl Drop for NetDevice {
    fn drop(&mut self) {
        for slot in &self.slots {
            unsafe {
                crate::os::mem::dma::kernel_dma_op().unmap_single(slot.map);
            }
        }
    }
}

struct NetRxToken {
    data: Vec<u8>,
}

impl RxToken for NetRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.data)
    }
}

struct NetTxToken<'a> {
    tx: &'a mut dyn ITxQueue,
}

impl<'a> TxToken for NetTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let ret = f(&mut buffer);

        let req_id = loop {
            match self
                .tx
                .submit_request(rdif_eth::TxRequest { data: &buffer })
            {
                Ok(req_id) => break req_id,
                Err(NetError::Retry) => spin_delay(Duration::from_millis(1)),
                Err(e) => panic!("tx submit failed: {e:?}"),
            }
        };

        loop {
            match self.tx.poll_request(req_id) {
                Ok(()) => break,
                Err(NetError::Retry) => spin_delay(Duration::from_millis(1)),
                Err(e) => panic!("tx poll failed: {e:?}"),
            }
        }

        ret
    }
}

impl Device for NetDevice {
    type RxToken<'a>
        = NetRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = NetTxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let data = self.poll_rx_packet()?;
        Some((
            NetRxToken { data },
            NetTxToken {
                tx: self.tx.as_mut(),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(NetTxToken {
            tx: self.tx.as_mut(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = self.tx.mtu();
        caps.medium = Medium::Ethernet;
        caps.max_burst_size = Some(1);
        caps
    }
}

pub fn run_ping_test(nic: &mut dyn Interface) {
    let mac = nic.mac_address();

    let tx = nic.create_tx_queue().expect("create tx queue");
    let rx = nic.create_rx_queue().expect("create rx queue");
    let mut dev = NetDevice::new(tx, rx);

    let config = Config::new(HardwareAddress::Ethernet(EthernetAddress::from_bytes(&mac)));
    let mut iface = SmolInterface::new(config, &mut dev, now());
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(LOCAL_IP, 24)).unwrap();
    });
    iface
        .routes_mut()
        .add_default_ipv4_route(GATEWAY_IP)
        .unwrap();

    let rx_buf = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 512]);
    let tx_buf = icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 512]);
    let icmp_socket = IcmpSocket::new(rx_buf, tx_buf);

    let mut sockets = SocketSet::new(vec![]);
    let icmp_handle = sockets.add(icmp_socket);

    let target = IpAddress::Ipv4(GATEWAY_IP);
    let ident = 0x22b;
    let mut sent = false;
    let mut received = false;

    for seq in 0u16..300 {
        let _ = iface.poll(now(), &mut dev, &mut sockets);

        let socket = sockets.get_mut::<IcmpSocket>(icmp_handle);
        if !socket.is_open() {
            socket.bind(icmp::Endpoint::Ident(ident)).unwrap();
        }

        if !sent && socket.can_send() {
            let repr = smoltcp::wire::Icmpv4Repr::EchoRequest {
                ident,
                seq_no: seq,
                data: b"sparreal ping",
            };
            let payload = socket.send(repr.buffer_len(), target).unwrap();
            let mut packet = smoltcp::wire::Icmpv4Packet::new_unchecked(payload);
            repr.emit(&mut packet, &dev.capabilities().checksum);
            sent = true;
            crate::println!("ping_test: icmp echo request sent");
        }

        if sent
            && socket.can_recv()
            && let Ok((_data, addr)) = socket.recv()
        {
            crate::println!("ping_test: icmp echo reply from {addr:?}");
            received = true;
            break;
        }

        spin_delay(Duration::from_millis(10));
    }

    assert!(received, "ping_test: no icmp echo reply received");
}
