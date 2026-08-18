extern crate alloc;

use alloc::{boxed::Box, string::String};
use core::{cell::Cell, net::Ipv4Addr, time::Duration};

use ax_net::{
    DeviceBinding, InterfaceConfig, InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind,
    InterfaceMatcher, Ipv4InterfaceConfig, NetError, NetResult, NetworkConfig, RouteInfo,
    StaticIpConfig,
    options::{
        Configurable, GetSocketOption, SetSocketOption, TcpInfo, TcpInfoOptions, TcpState,
        UnixCredentials,
    },
};
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};

#[test]
fn ax_net_interface_ids_bindings_and_config_snapshots_hold() {
    let id = InterfaceId::new(7);
    assert_eq!(id.get(), 7);
    assert_eq!(id.to_linux_ifindex(), 7);
    assert_eq!(InterfaceId::from_linux_ifindex(7), Some(id));
    assert_eq!(InterfaceId::from_linux_ifindex(0), None);
    assert_eq!(InterfaceId::from_linux_ifindex(-1), None);
    assert_eq!(InterfaceId::LOOPBACK.get(), 1);

    let binding = DeviceBinding { bound_if: Some(id) };
    assert_eq!(binding.bound_if, Some(id));
    assert_eq!(DeviceBinding::default().bound_if, None);

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
    assert_eq!(info.name, "eth7");
    assert!(info.flags.contains(InterfaceFlags::UP));
    assert!(info.flags.contains(InterfaceFlags::RUNNING));
    assert!(matches!(info.kind, InterfaceKind::Ethernet));
}

#[test]
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

    assert_eq!(cfg.interfaces[0].name, "eth0");
    assert!(matches!(
        cfg.interfaces[0].match_by,
        InterfaceMatcher::ByMac(EthernetAddress([0, 1, 2, 3, 4, 5]))
    ));
    assert_eq!(cfg.interfaces[0].static_ip.as_ref().unwrap().prefix_len, 24);
    assert!(!cfg.interfaces[0].dhcp);
    assert_eq!(cfg.default_dns_servers[0], Ipv4Addr::new(8, 8, 8, 8));

    let route = RouteInfo {
        filter: IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(0, 0, 0, 0), 0)),
        via: Some(IpAddress::Ipv4(Ipv4Address::new(192, 168, 1, 1))),
        interface_id: InterfaceId::new(2),
        source: IpAddress::Ipv4(Ipv4Address::new(192, 168, 1, 9)),
        metric: 10,
    };
    assert_eq!(route.metric, 10);
    assert_eq!(route.interface_id, InterfaceId::new(2));
}

struct MockConfigurable {
    supported: bool,
    set_calls: Cell<usize>,
}

impl Configurable for MockConfigurable {
    fn get_option_inner(&self, opt: &mut GetSocketOption) -> NetResult<bool> {
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

    fn set_option_inner(&self, opt: SetSocketOption) -> NetResult<bool> {
        if matches!(
            opt,
            SetSocketOption::NoDelay(true) | SetSocketOption::KeepAlive(true)
        ) {
            self.set_calls.set(self.set_calls.get() + 1);
        }
        Ok(self.supported)
    }
}

#[test]
fn ax_net_socket_options_dispatch_supported_and_unsupported_results() {
    let configurable = MockConfigurable {
        supported: true,
        set_calls: Cell::new(0),
    };

    let mut reuse = false;
    configurable
        .get_option(GetSocketOption::ReuseAddress(&mut reuse))
        .unwrap();
    assert!(reuse);

    let mut timeout = Duration::ZERO;
    configurable
        .get_option(GetSocketOption::SendTimeout(&mut timeout))
        .unwrap();
    assert_eq!(timeout, Duration::from_millis(7));

    let mut tcp_info = TcpInfo::default();
    configurable
        .get_option(GetSocketOption::TcpInfo(&mut tcp_info))
        .unwrap();
    assert_eq!(tcp_info.state, TcpState::Established);
    assert!(tcp_info.options.contains(TcpInfoOptions::SACK));
    assert_eq!(tcp_info.snd_mss, 1460);

    configurable
        .set_option(SetSocketOption::NoDelay(&true))
        .unwrap();
    configurable
        .set_option(SetSocketOption::KeepAlive(&true))
        .unwrap();
    assert_eq!(configurable.set_calls.get(), 2);

    let boxed: Box<dyn Configurable> = Box::new(MockConfigurable {
        supported: true,
        set_calls: Cell::new(0),
    });
    boxed.set_option(SetSocketOption::NoDelay(&true)).unwrap();

    let unsupported = MockConfigurable {
        supported: false,
        set_calls: Cell::new(0),
    };
    assert!(
        matches!(
            unsupported.set_option(SetSocketOption::NoDelay(&true)),
            Err(NetError::Unsupported)
        ) || unsupported
            .set_option(SetSocketOption::NoDelay(&true))
            .is_err()
    );
    let mut reuse = false;
    assert_eq!(
        unsupported
            .get_option(GetSocketOption::ReuseAddress(&mut reuse))
            .unwrap_err(),
        NetError::ProtocolOptionUnsupported
    );
}

#[test]
fn ax_net_tcp_info_credentials_and_option_payloads_keep_values() {
    let creds = UnixCredentials::new(42);
    assert_eq!(creds.pid, 42);
    assert_eq!(creds.uid, 0);
    assert_eq!(creds.gid, 0);

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
    assert_eq!(states[0], TcpState::Closed);
    assert_eq!(states[10], TcpState::TimeWait);

    let opts = TcpInfoOptions::TIMESTAMPS
        | TcpInfoOptions::SACK
        | TcpInfoOptions::WSCALE
        | TcpInfoOptions::ECN
        | TcpInfoOptions::ECN_SEEN
        | TcpInfoOptions::SYN_DATA;
    assert!(opts.contains(TcpInfoOptions::TIMESTAMPS));
    assert!(opts.contains(TcpInfoOptions::SYN_DATA));
}

#[test]
fn ax_net_interface_flags_hold() {
    use ax_net::InterfaceFlags;

    let flags = InterfaceFlags::empty();
    assert!(flags.is_empty());

    let up = InterfaceFlags::UP;
    assert!(!up.is_empty());

    let combined = up | InterfaceFlags::RUNNING;
    assert!(combined.contains(InterfaceFlags::UP));
    assert!(combined.contains(InterfaceFlags::RUNNING));
}

#[test]
fn ax_net_route_info_hold() {
    use ax_net::RouteInfo;

    // Test RouteInfo construction
    let route = RouteInfo {
        filter: IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::new(192, 168, 1, 0), 24)),
        via: Some(IpAddress::Ipv4(Ipv4Address::new(192, 168, 1, 1))),
        interface_id: InterfaceId::new(1),
        source: IpAddress::Ipv4(Ipv4Address::new(10, 0, 0, 1)),
        metric: 100,
    };

    assert_eq!(route.metric, 100);
    assert_eq!(route.interface_id.get(), 1);
}
