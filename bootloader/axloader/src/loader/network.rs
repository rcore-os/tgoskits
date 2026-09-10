extern crate alloc;

use alloc::vec::Vec;
use core::{ffi::c_void, ptr, slice, time::Duration};

use axloader::network_policy::{DiscoverySelectionError, select_unique_server};
use httpboot_protocol::{
    LoaderDiscoveryOffer, LoaderDiscoveryProbe, MAX_DISCOVERY_DATAGRAM_BYTES, MacAddress,
};
use uefi::{
    Event, Handle, Status, StatusExt,
    boot::{self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, Tpl},
    proto::{
        network::{http::HttpBinding, ip4config2::Ip4Config2, snp::SimpleNetwork},
        unsafe_protocol,
    },
};
use uefi_raw::{Boolean, Ipv4Address, protocol::driver::ServiceBindingProtocol, time::Time};

const UDP4_PROTOCOL_GUID: uefi::Guid = uefi::guid!("3ad9df29-4501-478d-b1f8-7f7fe70e50f3");
const UDP4_SERVICE_BINDING_GUID: uefi::Guid = uefi::guid!("83f01464-99bd-45e5-b383-af6305d8e9e6");
const UDP_POLL_STALL: Duration = Duration::from_millis(10);
const UDP_COMPLETION_POLLS: usize = 200;
const DISCOVERY_RECEIVE_ATTEMPTS: usize = 8;
const DISCOVERY_CLIENT_PORT: u16 = 2999;
const UDP_NETWORK_UNREACHABLE: Status = Status(Status::ERROR_BIT | 100);
const UDP_HOST_UNREACHABLE: Status = Status(Status::ERROR_BIT | 101);
const UDP_PROTOCOL_UNREACHABLE: Status = Status(Status::ERROR_BIT | 102);
const UDP_PORT_UNREACHABLE: Status = Status(Status::ERROR_BIT | 103);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    NoCompatibleInterface,
    InterfaceUnavailable,
    InvalidMac,
    UdpUnavailable,
    UdpConfigure,
    UdpTransmit,
    UdpReceive,
    Timeout,
    MalformedResponse,
    MultipleServers,
}

#[derive(Debug, Clone, Copy)]
pub struct NetworkInterface {
    handle: Handle,
    pub mac_address: MacAddress,
    pub current_mac_address: MacAddress,
    pub station_address: Ipv4Address,
    broadcast_address: Ipv4Address,
}

impl NetworkInterface {
    pub fn select() -> Result<Self, NetworkError> {
        let handles =
            boot::find_handles::<HttpBinding>().map_err(|_| NetworkError::NoCompatibleInterface)?;
        for handle in handles {
            let Ok(mut ip4) = Ip4Config2::new(handle) else {
                continue;
            };
            let Ok(udp_binding) = open_protocol::<Udp4Binding>(handle) else {
                continue;
            };
            let Ok(snp) = open_protocol::<SimpleNetwork>(handle) else {
                continue;
            };
            if snp.mode().hw_address_size != 6 {
                continue;
            }
            let Ok(permanent) = ethernet_mac(&snp.mode().permanent_address.0) else {
                continue;
            };
            let Ok(current) = ethernet_mac(&snp.mode().current_address.0) else {
                continue;
            };
            drop(snp);
            drop(udp_binding);
            if ip4.ifup().is_err() {
                continue;
            }
            let Ok(info) = ip4.get_interface_info() else {
                continue;
            };
            let mac_address = if permanent.is_zero() {
                current
            } else {
                permanent
            };
            if mac_address.is_zero() || current.is_zero() {
                continue;
            }
            return Ok(Self {
                handle,
                mac_address,
                current_mac_address: current,
                station_address: info.station_addr,
                broadcast_address: subnet_broadcast(info.station_addr, info.subnet_mask),
            });
        }
        Err(NetworkError::NoCompatibleInterface)
    }

    pub const fn handle(self) -> Handle {
        self.handle
    }

    pub fn discover_server(
        self,
        probe: &LoaderDiscoveryProbe,
    ) -> Result<LoaderDiscoveryOffer, NetworkError> {
        let payload = httpboot_protocol::encode_discovery_probe(probe)
            .map_err(|_| NetworkError::MalformedResponse)?;
        let mut udp = Udp4Client::new(self.handle)?;
        let first = udp.transmit_and_receive(
            self.broadcast_address,
            httpboot_protocol::DISCOVERY_PORT,
            &payload,
            MAX_DISCOVERY_DATAGRAM_BYTES,
        )?;

        let mut offers = Vec::new();
        offers.push(serde_json::from_slice(&first).map_err(|_| NetworkError::MalformedResponse)?);
        for _ in 1..DISCOVERY_RECEIVE_ATTEMPTS {
            let bytes = match udp.receive(MAX_DISCOVERY_DATAGRAM_BYTES) {
                Ok(bytes) => bytes,
                Err(NetworkError::Timeout) => continue,
                Err(error) => return Err(error),
            };
            let offer: LoaderDiscoveryOffer =
                serde_json::from_slice(&bytes).map_err(|_| NetworkError::MalformedResponse)?;
            offers.push(offer);
        }
        select_unique_server(offers).map_err(|error| match error {
            DiscoverySelectionError::NoCompatibleServer => NetworkError::Timeout,
            DiscoverySelectionError::MultipleServers => NetworkError::MultipleServers,
        })
    }
}

fn subnet_broadcast(station: Ipv4Address, mask: Ipv4Address) -> Ipv4Address {
    let mut address = [0; 4];
    for (index, octet) in address.iter_mut().enumerate() {
        *octet = station.0[index] | !mask.0[index];
    }
    Ipv4Address(address)
}

fn ethernet_mac(bytes: &[u8; 32]) -> Result<MacAddress, NetworkError> {
    let octets: [u8; 6] = bytes[..6]
        .try_into()
        .map_err(|_| NetworkError::InvalidMac)?;
    Ok(MacAddress::new(octets))
}

fn open_protocol<P: uefi::proto::ProtocolPointer>(
    handle: Handle,
) -> uefi::Result<ScopedProtocol<P>> {
    // SAFETY: The scoped guard closes the protocol before the selected UEFI
    // network controller can be destroyed or ExitBootServices is called.
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams {
                handle,
                agent: boot::image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}

#[derive(Debug)]
#[unsafe_protocol(UDP4_SERVICE_BINDING_GUID)]
struct Udp4Binding(ServiceBindingProtocol);

impl Udp4Binding {
    fn create_child(&mut self) -> uefi::Result<Handle> {
        let mut child = ptr::null_mut();
        let status = unsafe { (self.0.create_child)(&mut self.0, &mut child) };
        status.to_result_with_val(|| {
            // UEFI requires a successful CreateChild call to return a handle.
            unsafe { Handle::from_ptr(child) }.expect("UDP4 CreateChild returned a null handle")
        })
    }

    fn destroy_child(&mut self, child: Handle) -> uefi::Result<()> {
        unsafe { (self.0.destroy_child)(&mut self.0, child.as_ptr()) }.to_result()
    }
}

#[derive(Debug)]
#[unsafe_protocol(UDP4_PROTOCOL_GUID)]
struct Udp4(Udp4Protocol);

impl Udp4 {
    fn configure(&mut self, config: Option<&mut Udp4ConfigData>) -> uefi::Result<()> {
        let config = config.map_or(ptr::null_mut(), ptr::from_mut);
        unsafe { (self.0.configure)(&mut self.0, config) }.to_result()
    }

    fn transmit(&mut self, token: &mut Udp4CompletionToken) -> uefi::Result<()> {
        unsafe { (self.0.transmit)(&mut self.0, token) }.to_result()
    }

    fn receive(&mut self, token: &mut Udp4CompletionToken) -> uefi::Result<()> {
        unsafe { (self.0.receive)(&mut self.0, token) }.to_result()
    }

    fn cancel(&mut self, token: &mut Udp4CompletionToken) -> uefi::Result<()> {
        unsafe { (self.0.cancel)(&mut self.0, token) }.to_result()
    }

    fn poll(&mut self) {
        let _ = unsafe { (self.0.poll)(&mut self.0) };
    }
}

struct Udp4Client {
    child: Handle,
    binding: ScopedProtocol<Udp4Binding>,
    protocol: Option<ScopedProtocol<Udp4>>,
}

impl Udp4Client {
    fn new(nic_handle: Handle) -> Result<Self, NetworkError> {
        let mut binding =
            open_protocol::<Udp4Binding>(nic_handle).map_err(|_| NetworkError::UdpUnavailable)?;
        let child = binding
            .create_child()
            .map_err(|_| NetworkError::UdpUnavailable)?;
        let protocol = match open_protocol::<Udp4>(child) {
            Ok(protocol) => protocol,
            Err(_) => {
                let _ = binding.destroy_child(child);
                return Err(NetworkError::UdpUnavailable);
            }
        };
        let mut client = Self {
            child,
            binding,
            protocol: Some(protocol),
        };
        let mut config = Udp4ConfigData {
            accept_broadcast: true.into(),
            accept_promiscuous: false.into(),
            accept_any_port: false.into(),
            allow_duplicate_port: false.into(),
            type_of_service: 0,
            time_to_live: 16,
            do_not_fragment: true.into(),
            receive_timeout: 1_000_000,
            transmit_timeout: 1_000_000,
            use_default_address: true.into(),
            station_address: Ipv4Address::default(),
            subnet_mask: Ipv4Address::default(),
            station_port: DISCOVERY_CLIENT_PORT,
            remote_address: Ipv4Address::default(),
            remote_port: 0,
        };
        client
            .protocol_mut()
            .configure(Some(&mut config))
            .map_err(|_| NetworkError::UdpConfigure)?;
        Ok(client)
    }

    fn transmit(
        &mut self,
        destination: Ipv4Address,
        port: u16,
        payload: &[u8],
    ) -> Result<(), NetworkError> {
        let event = CompletionEvent::new()?;
        let mut session = Udp4SessionData {
            source_address: Ipv4Address::default(),
            source_port: DISCOVERY_CLIENT_PORT,
            destination_address: destination,
            destination_port: port,
        };
        let mut packet = Udp4TransmitData::<1> {
            udp_session_data: &mut session,
            gateway_address: ptr::null_mut(),
            data_length: payload
                .len()
                .try_into()
                .map_err(|_| NetworkError::UdpTransmit)?,
            fragment_count: 1,
            fragment_table: [Udp4FragmentData {
                fragment_length: payload
                    .len()
                    .try_into()
                    .map_err(|_| NetworkError::UdpTransmit)?,
                fragment_buffer: payload.as_ptr().cast::<c_void>().cast_mut(),
            }],
        };
        let mut token = Udp4CompletionToken {
            event: event.as_raw(),
            status: Status::NOT_READY,
            packet: Udp4CompletionTokenPacket {
                tx_data: ptr::from_mut(&mut packet).cast::<Udp4TransmitData>(),
            },
        };
        self.protocol_mut()
            .transmit(&mut token)
            .map_err(|_| NetworkError::UdpTransmit)?;
        self.wait(&mut token, NetworkError::UdpTransmit)
    }

    fn transmit_and_receive(
        &mut self,
        destination: Ipv4Address,
        port: u16,
        payload: &[u8],
        receive_limit: usize,
    ) -> Result<Vec<u8>, NetworkError> {
        let receive_event = CompletionEvent::new()?;
        let mut receive_token = Udp4CompletionToken {
            event: receive_event.as_raw(),
            status: Status::NOT_READY,
            packet: Udp4CompletionTokenPacket {
                rx_data: ptr::null_mut(),
            },
        };
        self.protocol_mut()
            .receive(&mut receive_token)
            .map_err(|_| NetworkError::UdpReceive)?;

        if let Err(error) = self.transmit(destination, port, payload) {
            self.cancel_and_complete(&mut receive_token, NetworkError::UdpReceive)?;
            return Err(error);
        }
        self.wait_for_receive(&mut receive_token)?;
        collect_received_bytes(&receive_token, receive_limit)
    }

    fn receive(&mut self, limit: usize) -> Result<Vec<u8>, NetworkError> {
        let event = CompletionEvent::new()?;
        let mut token = Udp4CompletionToken {
            event: event.as_raw(),
            status: Status::NOT_READY,
            packet: Udp4CompletionTokenPacket {
                rx_data: ptr::null_mut(),
            },
        };
        self.protocol_mut()
            .receive(&mut token)
            .map_err(|_| NetworkError::UdpReceive)?;
        self.wait_for_receive(&mut token)?;
        collect_received_bytes(&token, limit)
    }

    fn protocol_mut(&mut self) -> &mut Udp4 {
        self.protocol.as_mut().expect("UDP4 protocol is open")
    }

    fn wait(
        &mut self,
        token: &mut Udp4CompletionToken,
        error: NetworkError,
    ) -> Result<(), NetworkError> {
        for _ in 0..UDP_COMPLETION_POLLS {
            if token.status != Status::NOT_READY {
                return if token.status == Status::SUCCESS {
                    Ok(())
                } else {
                    crate::logln!("udp_completion_error: {:?}", token.status);
                    Err(error)
                };
            }
            self.protocol_mut().poll();
            boot::stall(UDP_POLL_STALL);
        }
        self.cancel_and_complete(token, error)?;
        Err(NetworkError::Timeout)
    }

    fn wait_for_receive(&mut self, token: &mut Udp4CompletionToken) -> Result<(), NetworkError> {
        for _ in 0..UDP_COMPLETION_POLLS {
            if token.status == Status::SUCCESS {
                return Ok(());
            }
            if token.status != Status::NOT_READY {
                if !is_discovery_icmp_status(token.status) {
                    crate::logln!("udp_completion_error: {:?}", token.status);
                    return Err(NetworkError::UdpReceive);
                }

                // A broadcast probe can also reach a network path without a
                // discovery listener.  Its ICMP error must not win the race
                // against a valid offer arriving through another path.
                crate::logln!("udp_discovery_ignored_error: {:?}", token.status);
                token.status = Status::NOT_READY;
                token.packet = Udp4CompletionTokenPacket {
                    rx_data: ptr::null_mut(),
                };
                self.protocol_mut()
                    .receive(token)
                    .map_err(|_| NetworkError::UdpReceive)?;
            }
            self.protocol_mut().poll();
            boot::stall(UDP_POLL_STALL);
        }
        self.cancel_and_complete(token, NetworkError::UdpReceive)?;
        Err(NetworkError::Timeout)
    }

    fn cancel_and_complete(
        &mut self,
        token: &mut Udp4CompletionToken,
        error: NetworkError,
    ) -> Result<(), NetworkError> {
        if token.status != Status::NOT_READY {
            return Ok(());
        }
        self.protocol_mut().cancel(token).map_err(|_| error)?;
        // EFI_UDP4_PROTOCOL.Cancel completes the token by signaling its event.
        // Do not let the stack-backed token or packet buffers go out of scope
        // until firmware has stopped referencing them.
        while token.status == Status::NOT_READY {
            self.protocol_mut().poll();
            boot::stall(UDP_POLL_STALL);
        }
        Ok(())
    }
}

fn is_discovery_icmp_status(status: Status) -> bool {
    status == UDP_NETWORK_UNREACHABLE
        || status == UDP_HOST_UNREACHABLE
        || status == UDP_PROTOCOL_UNREACHABLE
        || status == UDP_PORT_UNREACHABLE
        || status == Status::ICMP_ERROR
}

fn collect_received_bytes(
    token: &Udp4CompletionToken,
    limit: usize,
) -> Result<Vec<u8>, NetworkError> {
    let Some(receive) = (unsafe { token.packet.rx_data.as_ref() }) else {
        crate::logln!("udp_receive_error: missing RxData");
        return Err(NetworkError::UdpReceive);
    };
    let expected_length = receive.data_length as usize;
    let fragment_count: usize = receive
        .fragment_count
        .try_into()
        .map_err(|_| NetworkError::UdpReceive)?;
    if fragment_count == 0 || fragment_count > 32 || expected_length > limit {
        crate::logln!("udp_receive_error: invalid receive metadata");
        signal_recycle(receive.recycle_signal);
        return Err(NetworkError::UdpReceive);
    }
    let fragments_ptr = ptr::addr_of!(receive.fragment_table).cast::<Udp4FragmentData>();
    let fragments = unsafe { slice::from_raw_parts(fragments_ptr, fragment_count) };
    let mut bytes = Vec::with_capacity(expected_length);
    for fragment in fragments {
        let length = fragment.fragment_length as usize;
        if fragment.fragment_buffer.is_null() || bytes.len().saturating_add(length) > limit {
            crate::logln!("udp_receive_error: invalid fragment length={length}");
            signal_recycle(receive.recycle_signal);
            return Err(NetworkError::UdpReceive);
        }
        let data = unsafe { slice::from_raw_parts(fragment.fragment_buffer.cast::<u8>(), length) };
        bytes.extend_from_slice(data);
    }
    signal_recycle(receive.recycle_signal);
    if bytes.len() != expected_length {
        crate::logln!(
            "udp_receive_error: assembled={} expected={}",
            bytes.len(),
            expected_length
        );
        return Err(NetworkError::UdpReceive);
    }
    Ok(bytes)
}

impl Drop for Udp4Client {
    fn drop(&mut self) {
        if let Some(protocol) = self.protocol.as_mut() {
            let _ = protocol.configure(None);
        }
        self.protocol = None;
        let _ = self.binding.destroy_child(self.child);
    }
}

struct CompletionEvent(Option<Event>);

impl CompletionEvent {
    fn new() -> Result<Self, NetworkError> {
        let event = unsafe { boot::create_event(EventType::empty(), Tpl::APPLICATION, None, None) }
            .map_err(|_| NetworkError::UdpUnavailable)?;
        Ok(Self(Some(event)))
    }

    fn as_raw(&self) -> uefi_raw::Event {
        self.0.as_ref().expect("completion event is open").as_ptr()
    }
}

impl Drop for CompletionEvent {
    fn drop(&mut self) {
        if let Some(event) = self.0.take() {
            let _ = boot::close_event(event);
        }
    }
}

fn signal_recycle(raw_event: uefi_raw::Event) {
    if let Some(event) = unsafe { Event::from_ptr(raw_event) } {
        let _ = boot::signal_event(&event);
    }
}

#[repr(C)]
#[derive(Debug)]
struct Udp4Protocol {
    get_mode_data: unsafe extern "efiapi" fn(
        *mut Self,
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut c_void,
    ) -> Status,
    configure: unsafe extern "efiapi" fn(*mut Self, *mut Udp4ConfigData) -> Status,
    groups: unsafe extern "efiapi" fn(*mut Self, Boolean, *mut Ipv4Address) -> Status,
    routes: unsafe extern "efiapi" fn(
        *mut Self,
        Boolean,
        *mut Ipv4Address,
        *mut Ipv4Address,
        *mut Ipv4Address,
    ) -> Status,
    transmit: unsafe extern "efiapi" fn(*mut Self, *mut Udp4CompletionToken) -> Status,
    receive: unsafe extern "efiapi" fn(*mut Self, *mut Udp4CompletionToken) -> Status,
    cancel: unsafe extern "efiapi" fn(*mut Self, *mut Udp4CompletionToken) -> Status,
    poll: unsafe extern "efiapi" fn(*mut Self) -> Status,
}

#[repr(C)]
struct Udp4ConfigData {
    accept_broadcast: Boolean,
    accept_promiscuous: Boolean,
    accept_any_port: Boolean,
    allow_duplicate_port: Boolean,
    type_of_service: u8,
    time_to_live: u8,
    do_not_fragment: Boolean,
    receive_timeout: u32,
    transmit_timeout: u32,
    use_default_address: Boolean,
    station_address: Ipv4Address,
    subnet_mask: Ipv4Address,
    station_port: u16,
    remote_address: Ipv4Address,
    remote_port: u16,
}

#[repr(C)]
struct Udp4SessionData {
    source_address: Ipv4Address,
    source_port: u16,
    destination_address: Ipv4Address,
    destination_port: u16,
}

#[repr(C)]
struct Udp4FragmentData {
    fragment_length: u32,
    fragment_buffer: *mut c_void,
}

#[repr(C)]
struct Udp4ReceiveData<const N: usize = 0> {
    time_stamp: Time,
    recycle_signal: uefi_raw::Event,
    udp_session: Udp4SessionData,
    data_length: u32,
    fragment_count: u32,
    fragment_table: [Udp4FragmentData; N],
}

#[repr(C)]
struct Udp4TransmitData<const N: usize = 0> {
    udp_session_data: *mut Udp4SessionData,
    gateway_address: *mut Ipv4Address,
    data_length: u32,
    fragment_count: u32,
    fragment_table: [Udp4FragmentData; N],
}

#[repr(C)]
union Udp4CompletionTokenPacket {
    rx_data: *mut Udp4ReceiveData,
    tx_data: *mut Udp4TransmitData,
}

#[repr(C)]
struct Udp4CompletionToken {
    event: uefi_raw::Event,
    status: Status,
    packet: Udp4CompletionTokenPacket,
}
