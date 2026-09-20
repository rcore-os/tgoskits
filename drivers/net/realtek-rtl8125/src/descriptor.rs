use rdif_eth::{TxChecksumOffload, TxNetworkProtocol, TxTransportProtocol};

pub const DESC_OWN: u32 = 1 << 31;
pub const RING_END: u32 = 1 << 30;
pub const FIRST_FRAG: u32 = 1 << 29;
pub const LAST_FRAG: u32 = 1 << 28;

const TX_TRANSPORT_OFFSET_SHIFT: u32 = 18;
const TX_TRANSPORT_OFFSET_MAX: u16 = 0x03ff;
const TX_IPV6_CHECKSUM: u32 = 1 << 28;
const TX_IPV4_CHECKSUM: u32 = 1 << 29;
const TX_TCP_CHECKSUM: u32 = 1 << 30;
const TX_UDP_CHECKSUM: u32 = 1 << 31;

pub const RX_RES: u32 = 1 << 21;
pub const RX_RUNT: u32 = 1 << 20;
pub const RX_CRC: u32 = 1 << 19;

const RX_PACKET_LEN_MASK: u32 = 0x3fff;
const ETH_FCS_LEN: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TxDesc {
    pub opts1: u32,
    pub opts2: u32,
    pub addr: u64,
}

impl TxDesc {
    pub fn new_cpu_owned(
        addr: u64,
        len: usize,
        ring_end: bool,
        checksum: Option<TxChecksumOffload>,
    ) -> Option<Self> {
        let mut opts1 = FIRST_FRAG | LAST_FRAG | len as u32;
        if ring_end {
            opts1 |= RING_END;
        }
        let opts2 = match checksum {
            None => 0,
            Some(checksum) if checksum.transport_offset <= TX_TRANSPORT_OFFSET_MAX => {
                let network = match checksum.network {
                    TxNetworkProtocol::Ipv4 => TX_IPV4_CHECKSUM,
                    TxNetworkProtocol::Ipv6 => TX_IPV6_CHECKSUM,
                };
                let transport = match checksum.transport {
                    TxTransportProtocol::Tcp => TX_TCP_CHECKSUM,
                    TxTransportProtocol::Udp => TX_UDP_CHECKSUM,
                };
                network
                    | transport
                    | (u32::from(checksum.transport_offset) << TX_TRANSPORT_OFFSET_SHIFT)
            }
            Some(_) => return None,
        };
        Some(Self { opts1, opts2, addr })
    }

    pub fn release_to_hw(mut self) -> Self {
        self.opts1 |= DESC_OWN;
        self
    }

    pub fn is_owned_by_hw(&self) -> bool {
        self.opts1 & DESC_OWN != 0
    }

    pub fn len(&self) -> usize {
        (self.opts1 & RX_PACKET_LEN_MASK) as usize
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RxDesc {
    pub opts1: u32,
    pub opts2: u32,
    pub addr: u64,
}

impl RxDesc {
    pub fn new_cpu_owned(addr: u64, len: usize, ring_end: bool) -> Self {
        let mut opts1 = len as u32;
        if ring_end {
            opts1 |= RING_END;
        }
        Self {
            opts1,
            opts2: 0,
            addr,
        }
    }

    pub fn release_to_hw(mut self) -> Self {
        self.opts1 |= DESC_OWN;
        self
    }

    pub fn is_owned_by_hw(&self) -> bool {
        self.opts1 & DESC_OWN != 0
    }

    pub fn has_error(&self) -> bool {
        self.opts1 & (RX_RES | RX_RUNT | RX_CRC) != 0
    }

    pub fn is_whole_packet(&self) -> bool {
        self.opts1 & (FIRST_FRAG | LAST_FRAG) == FIRST_FRAG | LAST_FRAG
    }

    pub fn packet_len(&self) -> usize {
        let len = (self.opts1 & RX_PACKET_LEN_MASK) as usize;
        len.saturating_sub(ETH_FCS_LEN)
    }
}

#[cfg(test)]
mod tests {
    use rdif_eth::{TxChecksumOffload, TxNetworkProtocol, TxTransportProtocol};

    use super::*;

    #[test]
    fn transmit_checksum_request_uses_csum_v2_descriptor_fields() {
        let desc = TxDesc::new_cpu_owned(
            0x1000,
            1500,
            false,
            Some(TxChecksumOffload {
                network: TxNetworkProtocol::Ipv4,
                transport: TxTransportProtocol::Tcp,
                transport_offset: 34,
            }),
        )
        .expect("the checksum request must fit the descriptor");

        assert_eq!(desc.opts2, (1 << 29) | (1 << 30) | (34 << 18));
    }

    #[test]
    fn transmit_checksum_request_rejects_unrepresentable_offset() {
        assert!(
            TxDesc::new_cpu_owned(
                0x1000,
                1500,
                false,
                Some(TxChecksumOffload {
                    network: TxNetworkProtocol::Ipv6,
                    transport: TxTransportProtocol::Udp,
                    transport_offset: 0x0400,
                }),
            )
            .is_none()
        );
    }
}
