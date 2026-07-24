use alloc::{boxed::Box, string::String};
use core::{cell::Cell, net::Ipv4Addr, time::Duration};

use ax_errno::{AxError, AxResult, LinuxError};
use axtest::prelude::*;
use smoltcp::{
    phy::PacketMeta,
    storage::PacketBuffer,
    time::Instant,
    wire::{
        EthernetAddress, IpAddress, IpCidr, IpProtocol, Ipv4Address, Ipv4Cidr, Ipv4Packet,
        Ipv4Repr, Ipv6Address, Ipv6Packet, Ipv6Repr,
    },
};

use crate::{
    DeviceBinding, InterfaceConfig, InterfaceFlags, InterfaceId, InterfaceKind, InterfaceMatcher,
    NetworkConfig, RouteInfo, StaticIpConfig,
    addr::{allocate_ephemeral_port, listen_addrs_conflict, mask_from_prefix},
    config::{DnsServerEntry, DnsSource, InterfaceInfo, Ipv4InterfaceConfig},
    device::{ArpEntry, Device},
    options::{
        Configurable, GetSocketOption, SetSocketOption, TcpInfo, TcpInfoOptions, TcpState,
        UnixCredentials,
    },
    rx_meta::{ReceivedTrafficClass, packet_meta_for_rx_packet, received_traffic_class},
    state::{State, StateLock},
};

#[axtest]
fn ax_net_interface_ids_bindings_and_config_snapshots_hold() {
    let id = InterfaceId::new(7);
    ax_assert_eq!(id.get(), 7);
    ax_assert_eq!(id.to_linux_ifindex(), 7);
    ax_assert_eq!(InterfaceId::from_linux_ifindex(7), Some(id));
    ax_assert_eq!(InterfaceId::from_linux_ifindex(0), None);
    ax_assert_eq!(InterfaceId::from_linux_ifindex(-1), None);
    ax_assert_eq!(InterfaceId::LOOPBACK.get(), 1);

    let binding = DeviceBinding { bound_if: Some(id) };
    ax_assert_eq!(binding.bound_if, Some(id));
    ax_assert_eq!(DeviceBinding::default().bound_if, None);

    let info = InterfaceInfo {
        id,
        name: String::from("eth7"),
        kind: InterfaceKind::Ethernet,
        mac: Some(EthernetAddress([1, 2, 3, 4, 5, 6])),
        ipv4: Some(Ipv4InterfaceConfig {
            address: Ipv4Cidr::new(Ipv4Address::new(10, 0, 0, 7), 24),
            gateway: Some(Ipv4Address::new(10, 0, 0, 1)),
        }),
        mtu: 1500,
        flags: InterfaceFlags::UP | InterfaceFlags::RUNNING | InterfaceFlags::MULTICAST,
        metric: 20,
    };
    ax_assert_eq!(info.name, "eth7");
    ax_assert!(info.flags.contains(InterfaceFlags::UP));
    ax_assert!(info.flags.contains(InterfaceFlags::RUNNING));
    ax_assert!(matches!(info.kind, InterfaceKind::Ethernet));
}

#[axtest]
fn ax_net_static_and_dynamic_network_config_values_are_stable() {
    let cfg = NetworkConfig {
        interfaces: alloc::vec![InterfaceConfig {
            name: String::from("eth0"),
            match_by: InterfaceMatcher::ByMac(EthernetAddress([0, 1, 2, 3, 4, 5])),
            static_ip: Some(StaticIpConfig {
                ip: Ipv4Addr::new(192, 168, 1, 9),
                prefix_len: 24,
                gateway: Ipv4Addr::new(192, 168, 1, 1),
            }),
            dhcp: false,
            metric: 10,
            dns_servers: alloc::vec![Ipv4Addr::new(1, 1, 1, 1)],
        }],
        default_dns_servers: alloc::vec![Ipv4Addr::new(8, 8, 8, 8)],
    };

    ax_assert_eq!(cfg.interfaces[0].name, "eth0");
    ax_assert!(matches!(
        cfg.interfaces[0].match_by,
        InterfaceMatcher::ByMac(EthernetAddress([0, 1, 2, 3, 4, 5]))
    ));
    ax_assert_eq!(cfg.interfaces[0].static_ip.as_ref().unwrap().prefix_len, 24);
    ax_assert!(!cfg.interfaces[0].dhcp);
    ax_assert_eq!(cfg.default_dns_servers[0], Ipv4Addr::new(8, 8, 8, 8));

    let dns = DnsServerEntry {
        server: Ipv4Address::new(9, 9, 9, 9),
        interface_id: InterfaceId::new(2),
        metric: 5,
        source: DnsSource::Static,
    };
    ax_assert_eq!(dns.source, DnsSource::Static);
    ax_assert_eq!(DnsSource::Dhcp, DnsSource::Dhcp);
    ax_assert_eq!(DnsSource::Fallback, DnsSource::Fallback);

    let route = RouteInfo {
        filter: IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(0, 0, 0, 0), 0)),
        via: Some(IpAddress::Ipv4(Ipv4Address::new(192, 168, 1, 1))),
        interface_id: InterfaceId::new(2),
        source: IpAddress::Ipv4(Ipv4Address::new(192, 168, 1, 9)),
        metric: 10,
    };
    ax_assert_eq!(route.metric, 10);
    ax_assert_eq!(route.interface_id, InterfaceId::new(2));
}

#[axtest]
fn ax_net_addr_helpers_handle_wildcards_masks_and_ephemeral_ports() {
    ax_assert!(listen_addrs_conflict(None, None));
    ax_assert!(listen_addrs_conflict(
        None,
        Some(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1)))
    ));
    ax_assert!(listen_addrs_conflict(
        Some(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1))),
        Some(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1)))
    ));
    ax_assert!(!listen_addrs_conflict(
        Some(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 1))),
        Some(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 2)))
    ));

    ax_assert_eq!(mask_from_prefix(0), Ipv4Address::new(0, 0, 0, 0));
    ax_assert_eq!(mask_from_prefix(8), Ipv4Address::new(255, 0, 0, 0));
    ax_assert_eq!(mask_from_prefix(24), Ipv4Address::new(255, 255, 255, 0));
    ax_assert_eq!(mask_from_prefix(33), Ipv4Address::new(255, 255, 255, 255));

    let allocated = allocate_ephemeral_port(|port| port >= 0xc000).unwrap();
    ax_assert!(allocated >= 0xc000);
    ax_assert!(matches!(
        allocate_ephemeral_port(|_| false),
        Err(AxError::AddrInUse)
    ));
}

#[axtest]
fn ax_net_rx_metadata_round_trips_ipv4_ipv6_and_rejects_plain_meta() {
    let ipv4 = Ipv4Repr {
        src_addr: Ipv4Address::new(127, 0, 0, 1),
        dst_addr: Ipv4Address::new(127, 0, 0, 1),
        next_header: IpProtocol::Udp,
        payload_len: 0,
        hop_limit: 64,
    };
    let mut ipv4_packet = [0; 20];
    {
        let mut packet = Ipv4Packet::new_unchecked(&mut ipv4_packet[..]);
        ipv4.emit(&mut packet, &Default::default());
        packet.set_dscp(0x0b);
        packet.set_ecn(0x02);
    }
    ax_assert_eq!(
        received_traffic_class(packet_meta_for_rx_packet(&ipv4_packet)),
        Some(ReceivedTrafficClass::Ipv4(0x2e))
    );

    let ipv6 = Ipv6Repr {
        src_addr: Ipv6Address::LOCALHOST,
        dst_addr: Ipv6Address::LOCALHOST,
        next_header: IpProtocol::Udp,
        payload_len: 0,
        hop_limit: 64,
    };
    let mut ipv6_packet = [0; 40];
    {
        let mut packet = Ipv6Packet::new_unchecked(&mut ipv6_packet[..]);
        ipv6.emit(&mut packet);
        packet.set_traffic_class(0x3f);
    }
    ax_assert_eq!(
        received_traffic_class(packet_meta_for_rx_packet(&ipv6_packet)),
        Some(ReceivedTrafficClass::Ipv6(0x3f))
    );

    ax_assert_eq!(received_traffic_class(PacketMeta::default()), None);
    let mut unknown_version = PacketMeta::default();
    unknown_version.id = 0xa700_9901;
    ax_assert_eq!(received_traffic_class(unknown_version), None);
    ax_assert_eq!(packet_meta_for_rx_packet(&[0xff, 0]).id, 0);
}

struct MockConfigurable {
    supported: bool,
    set_calls: Cell<usize>,
}

impl Configurable for MockConfigurable {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> AxResult<bool> {
        match opt {
            GetSocketOption::ReuseAddress(value) => **value = true,
            GetSocketOption::SendTimeout(value) => **value = Duration::from_millis(7),
            GetSocketOption::TcpInfo(value) => {
                **value = TcpInfo {
                    state: TcpState::Established,
                    options: TcpInfoOptions::SACK | TcpInfoOptions::TIMESTAMPS,
                    snd_mss: 1460,
                    rcv_mss: 1460,
                    ..TcpInfo::default()
                };
            }
            _ => {}
        }
        Ok(self.supported)
    }

    fn set_option_inner(&self, opt: SetSocketOption) -> AxResult<bool> {
        if matches!(
            opt,
            SetSocketOption::NoDelay(true) | SetSocketOption::KeepAlive(true)
        ) {
            self.set_calls.set(self.set_calls.get() + 1);
        }
        Ok(self.supported)
    }
}

#[axtest]
fn ax_net_socket_options_dispatch_supported_and_unsupported_results() {
    let configurable = MockConfigurable {
        supported: true,
        set_calls: Cell::new(0),
    };

    let mut reuse = false;
    configurable
        .get_option(GetSocketOption::ReuseAddress(&mut reuse))
        .unwrap();
    ax_assert!(reuse);

    let mut timeout = Duration::ZERO;
    configurable
        .get_option(GetSocketOption::SendTimeout(&mut timeout))
        .unwrap();
    ax_assert_eq!(timeout, Duration::from_millis(7));

    let mut tcp_info = TcpInfo::default();
    configurable
        .get_option(GetSocketOption::TcpInfo(&mut tcp_info))
        .unwrap();
    ax_assert_eq!(tcp_info.state, TcpState::Established);
    ax_assert!(tcp_info.options.contains(TcpInfoOptions::SACK));
    ax_assert_eq!(tcp_info.snd_mss, 1460);

    configurable
        .set_option(SetSocketOption::NoDelay(&true))
        .unwrap();
    configurable
        .set_option(SetSocketOption::KeepAlive(&true))
        .unwrap();
    ax_assert_eq!(configurable.set_calls.get(), 2);

    let boxed: Box<dyn Configurable> = Box::new(MockConfigurable {
        supported: true,
        set_calls: Cell::new(0),
    });
    boxed.set_option(SetSocketOption::NoDelay(&true)).unwrap();

    let unsupported = MockConfigurable {
        supported: false,
        set_calls: Cell::new(0),
    };
    ax_assert!(
        matches!(
            unsupported.set_option(SetSocketOption::NoDelay(&true)),
            Err(AxError::Unsupported)
        ) || unsupported
            .set_option(SetSocketOption::NoDelay(&true))
            .is_err()
    );
    let mut reuse = false;
    ax_assert_eq!(
        unsupported
            .get_option(GetSocketOption::ReuseAddress(&mut reuse))
            .unwrap_err(),
        AxError::from(LinuxError::ENOPROTOOPT)
    );
}

#[axtest]
fn ax_net_tcp_info_credentials_and_option_payloads_keep_values() {
    let creds = UnixCredentials::new(42);
    ax_assert_eq!(creds.pid, 42);
    ax_assert_eq!(creds.uid, 0);
    ax_assert_eq!(creds.gid, 0);

    let states = [
        TcpState::Closed,
        TcpState::Listen,
        TcpState::SynSent,
        TcpState::SynReceived,
        TcpState::Established,
        TcpState::FinWait1,
        TcpState::FinWait2,
        TcpState::CloseWait,
        TcpState::Closing,
        TcpState::LastAck,
        TcpState::TimeWait,
    ];
    ax_assert_eq!(states[0], TcpState::Closed);
    ax_assert_eq!(states[10], TcpState::TimeWait);

    let opts = TcpInfoOptions::TIMESTAMPS
        | TcpInfoOptions::SACK
        | TcpInfoOptions::WSCALE
        | TcpInfoOptions::ECN
        | TcpInfoOptions::ECN_SEEN
        | TcpInfoOptions::SYN_DATA;
    ax_assert!(opts.contains(TcpInfoOptions::TIMESTAMPS));
    ax_assert!(opts.contains(TcpInfoOptions::SYN_DATA));
}

#[axtest]
fn ax_net_state_lock_commits_success_and_rolls_back_errors() {
    let lock = StateLock::new(State::Idle);
    ax_assert_eq!(lock.get(), State::Idle);

    let guard = lock.lock(State::Idle).unwrap();
    ax_assert_eq!(lock.get(), State::Busy);
    let value = guard
        .transit(State::Connected, || Ok::<_, AxError>(9))
        .unwrap();
    ax_assert_eq!(value, 9);
    ax_assert_eq!(lock.get(), State::Connected);

    match lock.lock(State::Idle) {
        Ok(_) => panic!("locking an unexpected state must fail"),
        Err(state) => ax_assert_eq!(state, State::Connected),
    }
    lock.set(State::Idle);
    let guard = lock.lock(State::Idle).unwrap();
    let result = guard.transit(State::Listening, || Err::<(), _>(AxError::BadState));
    ax_assert_eq!(result, Err(AxError::BadState));
    ax_assert_eq!(lock.get(), State::Idle);

    ax_assert_eq!(State::try_from(0), Ok(State::Idle));
    ax_assert_eq!(State::try_from(5), Ok(State::Closed));
    ax_assert_eq!(State::try_from(6), Err(()));
}

struct DefaultDevice;

impl Device for DefaultDevice {
    fn name(&self) -> &str {
        "default-device"
    }

    fn recv(
        &mut self,
        _interface_id: InterfaceId,
        _buffer: &mut PacketBuffer<InterfaceId>,
        _timestamp: Instant,
        _snoop: &mut dyn FnMut(&[u8]),
    ) -> usize {
        0
    }

    fn send(&mut self, _next_hop: IpAddress, _packet: &[u8], _timestamp: Instant) -> usize {
        0
    }
}

#[axtest]
fn ax_net_device_defaults_report_no_deferred_work_or_readiness() {
    let mut device = DefaultDevice;

    ax_assert_eq!(device.name(), "default-device");
    ax_assert!(device.drain_deferred_tx().is_empty());
    ax_assert!(device.drain_deferred_rx().is_empty());
    ax_assert_eq!(device.drain_deferred_tx_errors(), 0);
    ax_assert_eq!(device.drain_deferred_tx_drops(), 0);
    ax_assert_eq!(device.drain_deferred_rx_errors(), 0);
    ax_assert_eq!(device.drain_deferred_rx_drops(), 0);
    ax_assert!(device.arp_entries(Instant::from_millis(1)).is_empty());
    ax_assert!(device.readiness_poll().is_none());
    device.set_ipv4_addr(Some(Ipv4Cidr::new(Ipv4Address::LOCALHOST, 8)));

    let entry = ArpEntry {
        ip_addr: [192, 168, 1, 1],
        hw_type: 1,
        flags: 2,
        hw_addr: [1, 2, 3, 4, 5, 6],
        device: String::from("eth0"),
    };
    ax_assert_eq!(entry.ip_addr, [192, 168, 1, 1]);
    ax_assert_eq!(entry.device, "eth0");
}

#[axtest]
fn ax_net_interface_flags_hold() {
    use crate::InterfaceFlags;

    let flags = InterfaceFlags::empty();
    ax_assert!(flags.is_empty());

    let up = InterfaceFlags::UP;
    ax_assert!(!up.is_empty());

    let combined = up | InterfaceFlags::RUNNING;
    ax_assert!(combined.contains(InterfaceFlags::UP));
    ax_assert!(combined.contains(InterfaceFlags::RUNNING));
}

#[axtest]
fn ax_net_route_info_hold() {
    use crate::RouteInfo;

    // Test RouteInfo construction
    let route = RouteInfo {
        filter: IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(192, 168, 1, 0), 24)),
        via: Some(IpAddress::Ipv4(Ipv4Address::new(192, 168, 1, 1))),
        interface_id: InterfaceId::new(1),
        source: IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1)),
        metric: 100,
    };

    ax_assert_eq!(route.metric, 100);
    ax_assert_eq!(route.interface_id.get(), 1);
}
