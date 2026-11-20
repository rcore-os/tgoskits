//! IPv4 control over one firmware SNP, without a kernel network runtime.

use alloc::vec;

use axloader::{http_control::MAX_DISCOVERY_DATAGRAM_BYTES, ota::OtaController, target::CURRENT};
use httpboot_protocol::{DEVICE_PROTOCOL_VERSION, LoaderAnnouncement, MacAddress};
use smoltcp::{
    iface::{Config, Interface, PollIngressSingleResult, SocketHandle, SocketSet},
    socket::{dhcpv4, tcp, udp},
    time::{Duration, Instant},
    wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr},
};
use uefi::{Handle, Status, boot, proto::network::snp::SimpleNetwork};

use super::{
    arch,
    boot_server::{BootExecution, BootServer},
    http::{Action, HttpConnection},
    identity::BootIdentity,
    serial::SerialBeacon,
    smbios,
};

mod admission;
mod clock;
mod connection;
mod device;

use admission::SynBackpressure;
use clock::Clock;
use device::Device;

const HTTP_PORT: u16 = 2999;
const DISCOVERY_CLIENT_PORT: u16 = 2997;
const TCP_BUFFER_BYTES: usize = 64 * 1024;

pub struct Network {
    pub mac_address: MacAddress,
    pub current_mac_address: MacAddress,
    device: Device,
    interface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    udp: SocketHandle,
    tcp: [SocketHandle; 2],
    address: Option<Ipv4Cidr>,
    next_announcement: Instant,
}

impl Network {
    pub fn open() -> uefi::Result<Self> {
        let clock = Clock::open()?;
        let mut retries = 0_u8;
        loop {
            let mut last_error = Status::NOT_FOUND.into();
            let mut retry_network = false;
            for handle in boot::find_handles::<SimpleNetwork>()? {
                let mut network = match Self::open_candidate(handle, clock) {
                    Ok(network) => network,
                    Err(error) => {
                        if retries < 3 {
                            crate::logln!(
                                "loader_snp_candidate_open_failed: handle={handle:?} \
                                 error={error:?}"
                            );
                        }
                        retry_network |=
                            matches!(error.status(), Status::NO_MEDIA | Status::TIMEOUT);
                        last_error = error;
                        continue;
                    }
                };
                match network.acquire_address() {
                    Ok(()) => return Ok(network),
                    Err(error) => {
                        if retries < 3 {
                            crate::logln!(
                                "loader_snp_candidate_dhcp_failed: handle={handle:?} \
                                 error={error:?}"
                            );
                        }
                        // Complete the same NIC quiescence used before handoff
                        // before releasing packet storage or opening the next NIC.
                        network.close()?;
                        retry_network |=
                            matches!(error.status(), Status::NO_MEDIA | Status::TIMEOUT);
                        last_error = error;
                    }
                }
            }
            if !retry_network {
                return Err(last_error);
            }
            if retries < 3 {
                crate::logln!("loader_snp_retry: no candidate acquired an address");
            }
            retries = retries.saturating_add(1);
            // Every candidate is closed before another pass. A late cable or
            // DHCP server must not return a healthy loader to its OTA launcher.
            boot::stall(core::time::Duration::from_secs(2));
        }
    }

    fn open_candidate(handle: Handle, clock: Clock) -> uefi::Result<Self> {
        let mut device = Device::open(handle, clock)?;
        let (mac_address, current_mac_address) = device.addresses()?;
        let mut config = Config::new(EthernetAddress(current_mac_address.octets()).into());
        // This seed randomizes transport sequence numbers, independently of the
        // externally visible boot identity owned by the caller.
        config.random_seed = current_mac_address
            .octets()
            .iter()
            .fold(device.now().total_millis() as u64, |seed, byte| {
                seed.rotate_left(8) ^ u64::from(*byte)
            });
        let now = device.now();
        let interface = Interface::new(config, &mut device, now);
        let mut sockets = SocketSet::new(vec![]);
        let dhcp = sockets.add(dhcpv4::Socket::new());
        let mut udp_socket = udp::Socket::new(
            udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 1],
                vec![0; MAX_DISCOVERY_DATAGRAM_BYTES],
            ),
            udp::PacketBuffer::new(
                vec![udp::PacketMetadata::EMPTY; 2],
                vec![0; MAX_DISCOVERY_DATAGRAM_BYTES * 2],
            ),
        );
        udp_socket
            .bind(DISCOVERY_CLIENT_PORT)
            .map_err(|_| Status::INVALID_PARAMETER)?;
        let udp = sockets.add(udp_socket);
        let mut tcp_listener = || -> uefi::Result<SocketHandle> {
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]),
                tcp::SocketBuffer::new(vec![0; TCP_BUFFER_BYTES]),
            );
            socket.set_timeout(Some(Duration::from_secs(30)));
            socket
                .listen(HTTP_PORT)
                .map_err(|_| Status::INVALID_PARAMETER)?;
            Ok(sockets.add(socket))
        };
        // Keep a listener while the preceding HTTP connection finishes its
        // FIN exchange. Business requests remain serial and storage is bounded.
        let tcp = [tcp_listener()?, tcp_listener()?];
        crate::logln!("loader_snp_open: mac={mac_address} current={current_mac_address}");
        Ok(Self {
            mac_address,
            current_mac_address,
            device,
            interface,
            sockets,
            dhcp,
            udp,
            tcp,
            address: None,
            next_announcement: Instant::from_millis(0),
        })
    }

    fn acquire_address(&mut self) -> uefi::Result<()> {
        let deadline = self.device.now() + Duration::from_secs(30);
        loop {
            // Initial DHCP and the serving loop share ingress, DHCP state
            // updates, and egress. Identity-dependent announcements start only
            // after the selected NIC has an address and run() creates serial.
            let now = self.poll_transport(None)?;
            self.poll_egress(now)?;
            if self.address.is_some() {
                return Ok(());
            }
            if now >= deadline {
                return Err(Status::TIMEOUT.into());
            }
            boot::stall(core::time::Duration::from_millis(1));
        }
    }

    pub fn poll(
        &mut self,
        ota: &mut Option<OtaController>,
        server: &mut BootServer,
        serial: &mut SerialBeacon,
    ) -> uefi::Result<Action> {
        self.drive(serial, server.epoch(), None)?;
        let Some(tcp) = self.tcp.iter().copied().find(|handle| {
            let socket = self.sockets.get::<tcp::Socket>(*handle);
            matches!(
                socket.state(),
                tcp::State::Established | tcp::State::CloseWait
            ) && socket.can_recv()
        }) else {
            return Ok(Action::None);
        };
        // An active transaction always occupies the first TCP slot. Its spare
        // listener must follow it in smoltcp's first-match packet dispatch.
        let tcp = self.promote_tcp(tcp);
        let mut connection = connection::Connection::new(self, serial, server.epoch().into(), tcp);
        let result = connection.handle_http(ota, server);
        match result {
            Ok(action) => {
                #[cfg(feature = "qemu-fail-202-write")]
                {
                    use core::sync::atomic::{AtomicBool, Ordering};
                    static RESPONSE_RESET_PENDING: AtomicBool = AtomicBool::new(true);
                    if matches!(&action, Action::Boot(_))
                        && server.status().is_some_and(|status| {
                            status.boot_id == "qemu-202-write-failure"
                                && RESPONSE_RESET_PENDING.swap(false, Ordering::SeqCst)
                        })
                    {
                        // A closed connection cannot confirm the response,
                        // even if the application already queued every byte.
                        connection.abort();
                        crate::logln!("loader_http_response_reset_injected");
                    }
                    if matches!(&action, Action::Reset)
                        && ota.as_ref().is_some_and(|ota| {
                            ota.protocol_state().pending_update_id.as_deref()
                                == Some("01234567-89ab-cdef-0123-456789abcdef")
                        })
                    {
                        connection.abort();
                        crate::logln!("loader_ota_response_reset_injected");
                    }
                }
                if let Err(error) = connection.finish() {
                    crate::logln!("loader_http_response_failed: {error:?}");
                    connection.abort();
                    self.device.health()?;
                    return Ok(Action::None);
                }
                Ok(action)
            }
            Err(error) => {
                crate::logln!("loader_http_error: {error:?}");
                connection.abort();
                Ok(Action::None)
            }
        }
    }

    /// Quiesces the NIC before any payload is entered. Failure is terminal in
    /// Device::drop, which retains all submitted packets while resetting.
    pub fn close(mut self) -> uefi::Result<()> {
        self.device.close()
    }

    fn drive(
        &mut self,
        serial: &mut SerialBeacon,
        epoch: &str,
        active_tcp: Option<SocketHandle>,
    ) -> uefi::Result<()> {
        serial.progress();
        let now = self.poll_transport(active_tcp)?;
        if let Some(address) = self.address
            && now >= self.next_announcement
            && self.sockets.get::<udp::Socket>(self.udp).can_send()
        {
            let state = serial.state.borrow();
            let announcement = LoaderAnnouncement {
                protocol_version: DEVICE_PROTOCOL_VERSION,
                mac_address: self.mac_address,
                current_mac_address: self.current_mac_address,
                arch: CURRENT.arch_name.into(),
                loader_version: env!("CARGO_PKG_VERSION").into(),
                boot_epoch: epoch.into(),
                http_port: HTTP_PORT,
                serial_id: Some(state.serial_id.clone()),
                serial_ready: state.ready,
            };
            let bytes = serde_json::to_vec(&announcement).map_err(|_| Status::ABORTED)?;
            if bytes.len() > MAX_DISCOVERY_DATAGRAM_BYTES {
                return Err(Status::BAD_BUFFER_SIZE.into());
            }
            let destination = address.broadcast().unwrap_or(Ipv4Address::BROADCAST);
            self.sockets
                .get_mut::<udp::Socket>(self.udp)
                .send_slice(
                    &bytes,
                    (
                        IpAddress::Ipv4(destination),
                        axloader::http_control::DISCOVERY_PORT,
                    ),
                )
                .map_err(|_| Status::NOT_READY)?;
            self.next_announcement = now + Duration::from_secs(2);
        }
        self.poll_egress(now)
    }

    fn poll_transport(&mut self, active_tcp: Option<SocketHandle>) -> uefi::Result<Instant> {
        self.device.begin_poll()?;
        let now = self.device.now();
        self.interface.poll_maintenance(now);
        // Re-arm released sockets between ingress frames. Draining a whole RX
        // batch first would reject a new SYN queued immediately after a FIN/RST.
        // Device retains its finite RX budget across this entire drive call.
        loop {
            if active_tcp.is_some_and(|handle| !self.sockets.get::<tcp::Socket>(handle).is_active())
            {
                break;
            }
            self.listen(active_tcp)?;
            // With no listening socket, smoltcp would reject a fresh SYN with
            // RST. Delay only new handshakes until a slot is available; existing
            // connections must continue receiving their ACK/FIN/RST traffic.
            self.device
                .set_syn_backpressure(self.address.and_then(|address| {
                    SynBackpressure::when_full(
                        address.address(),
                        self.tcp
                            .map(|handle| self.sockets.get::<tcp::Socket>(handle)),
                    )
                }));
            if self
                .interface
                .poll_ingress_single(now, &mut self.device, &mut self.sockets)
                == PollIngressSingleResult::None
            {
                break;
            }
        }
        self.device.health()?;
        match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            Some(dhcpv4::Event::Configured(config)) => {
                self.interface.update_ip_addrs(|addresses| {
                    addresses.clear();
                    addresses
                        .push(IpCidr::Ipv4(config.address))
                        .expect("one IPv4 address fits");
                });
                if let Some(router) = config.router {
                    self.interface
                        .routes_mut()
                        .add_default_ipv4_route(router)
                        .map_err(|_| Status::OUT_OF_RESOURCES)?;
                } else {
                    self.interface.routes_mut().remove_default_ipv4_route();
                }
                self.address = Some(config.address);
                self.next_announcement = now;
                crate::logln!(
                    "loader_snp_dhcp: address={} router={:?}",
                    config.address,
                    config.router
                );
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.address = None;
                self.interface
                    .update_ip_addrs(|addresses| addresses.clear());
                self.interface.routes_mut().remove_default_ipv4_route();
                for handle in self.tcp {
                    self.sockets.get_mut::<tcp::Socket>(handle).abort();
                }
                crate::logln!("loader_snp_dhcp_lost");
            }
            None => {}
        }
        Ok(now)
    }

    fn poll_egress(&mut self, now: Instant) -> uefi::Result<()> {
        self.interface
            .poll_egress(now, &mut self.device, &mut self.sockets);
        self.device.health()
    }

    fn listen(&mut self, active_tcp: Option<SocketHandle>) -> uefi::Result<()> {
        for handle in self.tcp {
            // Resetting the current transaction would erase unacknowledged
            // response bytes and could assign its handle to a different peer.
            if Some(handle) == active_tcp {
                continue;
            }
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            if socket.state() == tcp::State::TimeWait
                || (socket.state() == tcp::State::Closed && socket.remote_endpoint().is_none())
            {
                // abort() leaves a pending RST with its old peer tuple. Let
                // poll_egress send it before listen() resets the socket.
                socket.listen(HTTP_PORT).map_err(|_| Status::DEVICE_ERROR)?;
            }
        }
        if active_tcp.is_none()
            && self.sockets.get::<tcp::Socket>(self.tcp[0]).is_listening()
            && self
                .sockets
                .get::<tcp::Socket>(self.tcp[1])
                .remote_endpoint()
                .is_some()
        {
            // A listener also accepts a retransmitted SYN for another socket.
            // Prefer the existing peer tuple before falling back to a listener.
            self.promote_tcp(self.tcp[1]);
        }
        Ok(())
    }

    fn promote_tcp(&mut self, handle: SocketHandle) -> SocketHandle {
        if handle != self.tcp[0] {
            // There is no borrowed Connection when slots are reordered. Moving
            // complete sockets retains their TCP state and owned packet buffers;
            // add() rebuilds only per-slot neighbor scheduling metadata.
            let first = self.sockets.remove(self.tcp[0]);
            let second = self.sockets.remove(self.tcp[1]);
            self.tcp = [self.sockets.add(second), self.sockets.add(first)];
        }
        self.tcp[0]
    }
}

/// Runs the native EFI control plane over the current target's SNP clock.
pub(in crate::loader) fn run() -> Status {
    crate::logln!("loader_network_backend: snp");
    let mut network = match Network::open() {
        Ok(network) => network,
        Err(error) => {
            crate::logln!("loader_snp_unavailable: {error:?}");
            return error.status();
        }
    };
    let mut ota = OtaController::open();
    let mut identity_failures = 0_u8;
    let identity = loop {
        match BootIdentity::establish(network.mac_address) {
            Ok(identity) => break identity,
            Err(status) => {
                if identity_failures < 3 {
                    crate::logln!("loader_identity_unavailable: {status:?}");
                }
                if !arch::RETRY_IDENTITY_FAILURE {
                    return close_before_return(network, status);
                }
                identity_failures = identity_failures.saturating_add(1);
                boot::stall(core::time::Duration::from_secs(2));
            }
        }
    };
    let mut serial = SerialBeacon::new(identity.serial_id);
    let mut server = BootServer::new(
        identity.epoch,
        network.mac_address,
        network.current_mac_address,
        smbios::hardware_info(),
        serial.state.clone(),
    );
    crate::logln!(
        "loader_device_listening: port=2999 epoch={}",
        server.epoch()
    );
    loop {
        let action = match network.poll(&mut ota, &mut server, &mut serial) {
            // OTA commits before replying. Unlike a kernel handoff, its reset
            // cannot be cancelled by a response write or acknowledgement error.
            _ if ota.as_ref().is_some_and(OtaController::reboot_pending) => Action::Reset,
            Ok(action) => action,
            Err(error) => {
                crate::logln!("loader_snp_failed: {error:?}");
                return close_before_return(network, error.status());
            }
        };
        match action {
            Action::None => boot::stall(core::time::Duration::from_millis(1)),
            Action::Reset => {
                crate::logln!("ota_staged_reboot");
                if let Err(error) = network.close() {
                    crate::logln!("loader_snp_shutdown_failed: {error:?}");
                    reset(error.status());
                }
                drop(serial);
                reset(Status::SUCCESS);
            }
            Action::Boot(execution) => {
                let BootExecution { elf, handoff } = *execution;
                crate::logln!(
                    "elf_loaded: load={:#x} end={:#x} entry={:#x}",
                    elf.load_addr,
                    elf.load_end,
                    elf.entry_point
                );
                if let Err(error) = network.close() {
                    crate::logln!("loader_snp_shutdown_failed: {error:?}");
                    reset(error.status());
                }
                drop(serial);
                // Uploaded buffers have no role in the receiving kernel and
                // must be released before EBS. OTA only retains value data.
                drop(server);
                crate::logln!("ready_to_handoff");
                // SAFETY: validated ELF entry and allocation remain live; TCP
                // acknowledged the response, and SNP/UART resources are closed.
                // The shared handoff owner terminates instead of unwinding if a
                // child returns after invalidating Boot Services.
                let result = unsafe { handoff.enter(elf.entry_point) };
                drop(handoff);
                drop(elf);
                crate::logln!("jump_error: {result:?}");
                return Status::LOAD_ERROR;
            }
        }
    }
}

fn close_before_return(network: Network, status: Status) -> Status {
    if let Err(error) = network.close() {
        crate::logln!("loader_snp_shutdown_failed: {error:?}");
        reset(error.status());
    }
    status
}

fn reset(status: Status) -> ! {
    uefi::runtime::reset(uefi::runtime::ResetType::COLD, status, None)
}
