//! Bounded Ethernet forwarding policy for emulated guest ports.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Bytes required to inspect the destination, source, and EtherType fields.
pub const ETHERNET_HEADER_LEN: usize = 14;
/// Maximum number of ports accepted in one immutable topology snapshot.
pub const MAX_SWITCH_PORTS: usize = 256;

/// Snapshot-local identity of one emulated switch port.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PortId(pub usize);

/// Layer-2 isolation domain assigned to one or more ports.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SegmentId(pub u16);

/// Ethernet media access control address.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    /// Ethernet broadcast destination.
    pub const BROADCAST: Self = Self([0xff; 6]);

    /// Returns whether the group bit is set.
    pub const fn is_multicast(self) -> bool {
        self.0[0] & 1 != 0
    }

    /// Returns whether every address octet is zero.
    pub fn is_zero(self) -> bool {
        self.0 == [0; 6]
    }
}

/// Immutable identity and isolation policy for one switch port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitchPort {
    /// Unique identity within one topology snapshot.
    pub id: PortId,
    /// Isolation domain used for destination lookup and multicast replication.
    pub segment: SegmentId,
    /// Only valid source address for frames received from this port.
    pub mac: MacAddress,
}

/// Validated, bounded snapshot of all emulated switch ports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwitchTopology {
    ports: Vec<SwitchPort>,
}

impl SwitchTopology {
    /// Validates and owns a bounded port snapshot.
    pub fn new(ports: &[SwitchPort]) -> Result<Self, TopologyError> {
        if ports.len() > MAX_SWITCH_PORTS {
            return Err(TopologyError::TooManyPorts {
                count: ports.len(),
                max: MAX_SWITCH_PORTS,
            });
        }
        for (index, port) in ports.iter().enumerate() {
            if port.mac.is_zero() || port.mac.is_multicast() {
                return Err(TopologyError::InvalidUnicastMac {
                    port: port.id,
                    mac: port.mac,
                });
            }
            for existing in &ports[..index] {
                if existing.id == port.id {
                    return Err(TopologyError::DuplicatePortId(port.id));
                }
                if existing.segment == port.segment && existing.mac == port.mac {
                    return Err(TopologyError::DuplicateMac {
                        segment: port.segment,
                        mac: port.mac,
                    });
                }
            }
        }

        let mut owned_ports = Vec::new();
        owned_ports
            .try_reserve_exact(ports.len())
            .map_err(|_| TopologyError::AllocationFailed)?;
        owned_ports.extend_from_slice(ports);
        Ok(Self { ports: owned_ports })
    }

    /// Routes an Ethernet frame without retaining guest-controlled data.
    pub fn route(&self, ingress: PortId, frame: &[u8]) -> RouteDecision {
        let Some(ingress_port) = self.ports.iter().find(|port| port.id == ingress) else {
            return RouteDecision::Drop(DropReason::UnknownIngress);
        };
        if frame.len() < ETHERNET_HEADER_LEN {
            return RouteDecision::Drop(DropReason::FrameTooShort);
        }

        let destination = MacAddress([frame[0], frame[1], frame[2], frame[3], frame[4], frame[5]]);
        let source = MacAddress([frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]]);
        if source != ingress_port.mac {
            return RouteDecision::Drop(DropReason::SpoofedSource);
        }

        if destination.is_multicast() {
            let target_count = self
                .ports
                .iter()
                .filter(|port| port.segment == ingress_port.segment && port.id != ingress)
                .count();
            let mut targets = Vec::new();
            if targets.try_reserve_exact(target_count).is_err() {
                return RouteDecision::Drop(DropReason::ResourceExhausted);
            }
            targets.extend(
                self.ports
                    .iter()
                    .filter(|port| port.segment == ingress_port.segment && port.id != ingress)
                    .map(|port| port.id),
            );
            return RouteDecision::Forward {
                kind: ForwardKind::Multicast,
                targets,
            };
        }

        match self
            .ports
            .iter()
            .find(|port| port.segment == ingress_port.segment && port.mac == destination)
        {
            Some(port) if port.id == ingress => RouteDecision::Drop(DropReason::ReflectedUnicast),
            Some(port) => {
                let mut targets = Vec::new();
                if targets.try_reserve_exact(1).is_err() {
                    return RouteDecision::Drop(DropReason::ResourceExhausted);
                }
                targets.push(port.id);
                RouteDecision::Forward {
                    kind: ForwardKind::Unicast,
                    targets,
                }
            }
            None => RouteDecision::Drop(DropReason::UnknownUnicast),
        }
    }
}

/// Result of applying the forwarding policy to one frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteDecision {
    /// Replicate the frame to the selected ports.
    Forward {
        /// Destination classification used by observability counters.
        kind: ForwardKind,
        /// Exact snapshot-local ports that may receive the frame.
        targets: Vec<PortId>,
    },
    /// Discard the frame without delivering it to any port.
    Drop(DropReason),
}

/// Classification of a successfully resolved destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardKind {
    /// One exact destination MAC in the ingress segment.
    Unicast,
    /// Every other port in the ingress segment.
    Multicast,
}

/// Policy or resource reason for discarding one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropReason {
    /// The supplied ingress identity is absent from this topology snapshot.
    UnknownIngress,
    /// The frame does not contain a complete Ethernet header.
    FrameTooShort,
    /// The source address differs from the configured ingress address.
    SpoofedSource,
    /// No destination with this address exists in the ingress segment.
    UnknownUnicast,
    /// A port attempted to send a unicast frame back to itself.
    ReflectedUnicast,
    /// Memory for the bounded forwarding decision could not be reserved.
    ResourceExhausted,
}

/// Invalid or unrepresentable switch topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TopologyError {
    /// A snapshot exceeded [`MAX_SWITCH_PORTS`].
    #[error("switch topology contains {count} ports, exceeding the limit of {max}")]
    TooManyPorts {
        /// Number of ports supplied by the caller.
        count: usize,
        /// Maximum number of ports accepted by this crate.
        max: usize,
    },
    /// Two topology entries used the same snapshot-local identity.
    #[error("duplicate switch port identity {0:?}")]
    DuplicatePortId(PortId),
    /// Two ports in one isolation domain used the same MAC address.
    #[error("duplicate MAC address {mac:?} in segment {segment:?}")]
    DuplicateMac {
        /// Isolation domain containing the conflict.
        segment: SegmentId,
        /// Conflicting address.
        mac: MacAddress,
    },
    /// A port was configured with a zero or group MAC address.
    #[error("switch port {port:?} has invalid unicast MAC address {mac:?}")]
    InvalidUnicastMac {
        /// Port containing the invalid address.
        port: PortId,
        /// Invalid address.
        mac: MacAddress,
    },
    /// Memory for the owned topology snapshot could not be reserved.
    #[error("failed to allocate the switch topology snapshot")]
    AllocationFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORT_1: SwitchPort = SwitchPort {
        id: PortId(1),
        segment: SegmentId(7),
        mac: MacAddress([0x52, 0x54, 0, 0, 0, 1]),
    };
    const PORT_2: SwitchPort = SwitchPort {
        id: PortId(2),
        segment: SegmentId(7),
        mac: MacAddress([0x52, 0x54, 0, 0, 0, 2]),
    };
    const PORT_3: SwitchPort = SwitchPort {
        id: PortId(3),
        segment: SegmentId(8),
        mac: MacAddress([0x52, 0x54, 0, 0, 0, 3]),
    };

    fn topology() -> SwitchTopology {
        SwitchTopology::new(&[PORT_1, PORT_2, PORT_3]).unwrap()
    }

    fn ethernet_frame(destination: MacAddress, source: MacAddress) -> [u8; ETHERNET_HEADER_LEN] {
        let mut frame = [0u8; ETHERNET_HEADER_LEN];
        frame[..6].copy_from_slice(&destination.0);
        frame[6..12].copy_from_slice(&source.0);
        frame[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
        frame
    }

    #[test]
    fn known_unicast_reaches_only_destination_port() {
        assert_eq!(
            topology().route(PORT_1.id, &ethernet_frame(PORT_2.mac, PORT_1.mac)),
            RouteDecision::Forward {
                kind: ForwardKind::Unicast,
                targets: alloc::vec![PORT_2.id],
            }
        );
    }

    #[test]
    fn broadcast_stays_inside_ingress_segment() {
        assert_eq!(
            topology().route(
                PORT_1.id,
                &ethernet_frame(MacAddress::BROADCAST, PORT_1.mac)
            ),
            RouteDecision::Forward {
                kind: ForwardKind::Multicast,
                targets: alloc::vec![PORT_2.id],
            }
        );
    }

    #[test]
    fn non_broadcast_multicast_stays_inside_ingress_segment() {
        let multicast = MacAddress([0x01, 0x00, 0x5e, 0, 0, 1]);
        assert_eq!(
            topology().route(PORT_1.id, &ethernet_frame(multicast, PORT_1.mac)),
            RouteDecision::Forward {
                kind: ForwardKind::Multicast,
                targets: alloc::vec![PORT_2.id],
            }
        );
    }

    #[test]
    fn known_unicast_in_another_segment_is_dropped() {
        assert_eq!(
            topology().route(PORT_1.id, &ethernet_frame(PORT_3.mac, PORT_1.mac)),
            RouteDecision::Drop(DropReason::UnknownUnicast)
        );
    }

    #[test]
    fn reflected_unicast_is_dropped() {
        assert_eq!(
            topology().route(PORT_1.id, &ethernet_frame(PORT_1.mac, PORT_1.mac)),
            RouteDecision::Drop(DropReason::ReflectedUnicast)
        );
    }

    #[test]
    fn unknown_unicast_is_dropped_instead_of_flooded() {
        assert_eq!(
            topology().route(
                PORT_1.id,
                &ethernet_frame(MacAddress([0x52, 0x54, 0, 0, 0, 99]), PORT_1.mac)
            ),
            RouteDecision::Drop(DropReason::UnknownUnicast)
        );
    }

    #[test]
    fn spoofed_source_is_dropped() {
        assert_eq!(
            topology().route(PORT_1.id, &ethernet_frame(PORT_2.mac, PORT_3.mac)),
            RouteDecision::Drop(DropReason::SpoofedSource)
        );
    }

    #[test]
    fn duplicate_mac_in_one_segment_is_rejected() {
        let duplicate = SwitchPort {
            id: PortId(4),
            ..PORT_1
        };
        assert_eq!(
            SwitchTopology::new(&[PORT_1, duplicate]),
            Err(TopologyError::DuplicateMac {
                segment: PORT_1.segment,
                mac: PORT_1.mac,
            })
        );
    }

    #[test]
    fn duplicate_port_id_is_rejected_across_segments() {
        let duplicate = SwitchPort {
            id: PORT_1.id,
            ..PORT_3
        };
        assert_eq!(
            SwitchTopology::new(&[PORT_1, duplicate]),
            Err(TopologyError::DuplicatePortId(PORT_1.id))
        );
    }

    #[test]
    fn zero_and_multicast_port_macs_are_rejected() {
        for mac in [MacAddress([0; 6]), MacAddress::BROADCAST] {
            let invalid = SwitchPort { mac, ..PORT_1 };
            assert_eq!(
                SwitchTopology::new(&[invalid]),
                Err(TopologyError::InvalidUnicastMac {
                    port: invalid.id,
                    mac,
                })
            );
        }
    }

    #[test]
    fn identical_macs_in_different_segments_remain_isolated() {
        let isolated = SwitchPort {
            id: PortId(4),
            segment: PORT_3.segment,
            mac: PORT_1.mac,
        };
        let topology = SwitchTopology::new(&[PORT_1, PORT_2, isolated]).unwrap();
        assert_eq!(
            topology.route(PORT_2.id, &ethernet_frame(PORT_1.mac, PORT_2.mac)),
            RouteDecision::Forward {
                kind: ForwardKind::Unicast,
                targets: alloc::vec![PORT_1.id],
            }
        );
        assert_eq!(
            topology.route(isolated.id, &ethernet_frame(isolated.mac, isolated.mac)),
            RouteDecision::Drop(DropReason::ReflectedUnicast)
        );
    }

    #[test]
    fn oversized_topology_is_rejected() {
        let ports = (0..257)
            .map(|index| SwitchPort {
                id: PortId(index),
                segment: SegmentId(1),
                mac: MacAddress([
                    0x52,
                    0x54,
                    (index >> 16) as u8,
                    (index >> 8) as u8,
                    index as u8,
                    1,
                ]),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            SwitchTopology::new(&ports),
            Err(TopologyError::TooManyPorts {
                count: 257,
                max: MAX_SWITCH_PORTS,
            })
        );
    }
}
