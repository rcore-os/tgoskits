use alloc::{boxed::Box, vec::Vec};
use core::{
    pin::Pin,
    task::{Context, Waker},
};

use ax_hal::time::{NANOS_PER_MICROS, TimeValue, wall_time_nanos};
use ax_task::future::sleep_until;
use smoltcp::{
    iface::{Interface, SocketHandle, SocketSet},
    socket::dhcpv4,
    time::Instant,
    wire::{
        EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpListenEndpoint, Ipv4Address,
        Ipv4Cidr,
    },
};

use crate::{SOCKET_SET, router::Router};

fn now() -> Instant {
    Instant::from_micros_const((wall_time_nanos() / NANOS_PER_MICROS) as i64)
}

pub struct Service {
    pub iface: Interface,
    router: Router,
    timeout: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    dhcp: Option<DhcpState>,
}

struct DhcpState {
    handle: Option<SocketHandle>,
    dev: usize,
    mac: EthernetAddress,
    address: Option<Ipv4Cidr>,
    dns_servers: Vec<Ipv4Address>,
}
impl Service {
    pub fn new(mut router: Router) -> Self {
        let config = smoltcp::iface::Config::new(HardwareAddress::Ip);
        let iface = Interface::new(config, &mut router, now());

        Self {
            iface,
            router,
            timeout: None,
            dhcp: None,
        }
    }

    pub fn enable_dhcp(&mut self, dev: usize, mac: EthernetAddress) {
        self.dhcp = Some(DhcpState {
            handle: None,
            dev,
            mac,
            address: None,
            dns_servers: Vec::new(),
        });
        info!("eth0: DHCP enabled");
    }

    pub fn dhcp_enabled(&self) -> bool {
        self.dhcp.is_some()
    }

    pub fn dhcp_configured(&self) -> bool {
        self.dhcp
            .as_ref()
            .is_some_and(|state| state.address.is_some())
    }

    pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
        let timestamp = now();

        self.router.poll(timestamp);
        self.iface.poll(timestamp, &mut self.router, sockets);
        self.poll_dhcp(sockets);
        self.router.dispatch(timestamp)
    }

    fn poll_dhcp(&mut self, sockets: &mut SocketSet) {
        let Some(state) = &mut self.dhcp else {
            return;
        };
        let handle = *state.handle.get_or_insert_with(|| {
            let mut socket = dhcpv4::Socket::new();
            socket.set_client_hardware_address(state.mac);
            sockets.add(socket)
        });

        let event = sockets
            .get_mut::<dhcpv4::Socket>(handle)
            .poll()
            .map(|event| match event {
                dhcpv4::Event::Configured(config) => DhcpEvent::Configured {
                    address: config.address,
                    router: config.router,
                    dns_servers: config.dns_servers.iter().copied().collect(),
                },
                dhcpv4::Event::Deconfigured => DhcpEvent::Deconfigured,
            });

        match event {
            Some(DhcpEvent::Configured {
                address,
                router,
                dns_servers,
            }) => {
                info!("eth0: DHCP acquired address {address}");
                match router {
                    Some(router) => info!("eth0: DHCP router {router}"),
                    None => info!("eth0: DHCP router not provided"),
                }
                for dns in &dns_servers {
                    info!("eth0: DHCP DNS {dns}");
                }

                Self::set_interface_ipv4(&mut self.iface, state.address, Some(address));
                state.address = Some(address);
                state.dns_servers = dns_servers;
                self.router
                    .set_ipv4_config(state.dev, Some(address), router.map(IpAddress::Ipv4));
            }
            Some(DhcpEvent::Deconfigured) => {
                if state.address.is_some() {
                    info!("eth0: DHCP deconfigured");
                }
                Self::set_interface_ipv4(&mut self.iface, state.address, None);
                state.address = None;
                state.dns_servers.clear();
                self.router.set_ipv4_config(state.dev, None, None);
            }
            None => {}
        }
    }

    fn set_interface_ipv4(
        iface: &mut Interface,
        old_address: Option<Ipv4Cidr>,
        new_address: Option<Ipv4Cidr>,
    ) {
        iface.update_ip_addrs(|ip_addrs| {
            if let Some(old_address) = old_address {
                ip_addrs.retain(|addr| *addr != IpCidr::Ipv4(old_address));
            }
            if let Some(new_address) = new_address {
                let new_address = IpCidr::Ipv4(new_address);
                if !ip_addrs.contains(&new_address) {
                    ip_addrs.push(new_address).unwrap();
                }
            }
        });
    }

    pub fn get_source_address(&self, dst_addr: &IpAddress) -> IpAddress {
        let Some(rule) = self.router.table.lookup(dst_addr) else {
            panic!("no route to destination: {dst_addr}");
        };
        rule.src
    }

    pub fn device_mask_for(&self, endpoint: &IpListenEndpoint) -> u32 {
        match endpoint.addr {
            Some(addr) => self
                .router
                .table
                .lookup(&addr)
                .map_or(0, |it| 1u32 << it.dev),
            None => u32::MAX,
        }
    }

    pub fn register_waker(&mut self, mask: u32, waker: &Waker) {
        let next = self.iface.poll_at(now(), &SOCKET_SET.inner.lock());

        if let Some(t) = next {
            let next = TimeValue::from_micros(t.total_micros() as _);

            // drop old timeout future
            self.timeout = None;

            let mut fut = Box::pin(sleep_until(next));
            let mut cx = Context::from_waker(waker);

            if fut.as_mut().poll(&mut cx).is_ready() {
                waker.wake_by_ref();
                return;
            } else {
                self.timeout = Some(fut);
            }
        }

        for (i, device) in self.router.devices.iter().enumerate() {
            if mask & (1 << i) != 0 {
                device.register_waker(waker);
            }
        }
    }
}

enum DhcpEvent {
    Configured {
        address: Ipv4Cidr,
        router: Option<Ipv4Address>,
        dns_servers: Vec<Ipv4Address>,
    },
    Deconfigured,
}
