//! Bounded, single-connection UEFI TCP4 HTTP control endpoint.

extern crate alloc;

use alloc::boxed::Box;
use core::ptr;

use axloader::ota::OtaController;
use uefi::{
    Event, Handle, Status, StatusExt,
    boot::{self, EventType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, Tpl},
    proto::unsafe_protocol,
};
use uefi_raw::{
    Ipv4Address,
    protocol::{
        driver::ServiceBindingProtocol,
        network::tcp4::{
            Tcp4AccessPoint, Tcp4CompletionToken, Tcp4ConfigData, Tcp4FragmentData, Tcp4IoToken,
            Tcp4ListenToken, Tcp4Packet, Tcp4Protocol, Tcp4ReceiveData, Tcp4TransmitData,
        },
    },
};

use super::{boot_server::BootServer, http::HttpConnection};

const TCP4_BINDING_GUID: uefi::Guid = uefi::guid!("00720665-67eb-4a99-baf7-d3c33a1c7cc9");
const TCP4_GUID: uefi::Guid = uefi::guid!("65530bc7-a359-410f-b010-5aadc7ec2b62");
const IO_WAIT_POLLS: usize = 3000;

#[derive(Debug)]
#[unsafe_protocol(TCP4_BINDING_GUID)]
pub(super) struct TcpBinding(ServiceBindingProtocol);

#[derive(Debug)]
#[unsafe_protocol(TCP4_GUID)]
struct Tcp(Tcp4Protocol);

fn open_protocol<P: uefi::proto::ProtocolPointer>(
    handle: Handle,
) -> uefi::Result<ScopedProtocol<P>> {
    // SAFETY: Every guard is closed before the TCP child is destroyed.
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

fn completion_event() -> uefi::Result<Event> {
    // SAFETY: The event has no callback and is closed after its token completes.
    unsafe { boot::create_event(EventType::empty(), Tpl::APPLICATION, None, None) }
}

fn status_token(event: &Event) -> Tcp4CompletionToken {
    Tcp4CompletionToken {
        event: event.as_ptr(),
        status: Status::NOT_READY,
    }
}

pub struct Listener {
    binding: ScopedProtocol<TcpBinding>,
    listen_handle: Handle,
    protocol: Option<ScopedProtocol<Tcp>>,
    token: Box<Tcp4ListenToken>,
    event: Option<Event>,
}

pub use super::http::Action;

impl Listener {
    pub fn open(nic: Handle) -> uefi::Result<Self> {
        let mut binding = open_protocol::<TcpBinding>(nic)?;
        let mut raw = ptr::null_mut();
        // SAFETY: Child ownership is held until the protocol guard is closed.
        unsafe { (binding.0.create_child)(&mut binding.0, &mut raw) }.to_result()?;
        let listen_handle = unsafe { Handle::from_ptr(raw) }.ok_or(Status::DEVICE_ERROR)?;
        let protocol = match open_protocol::<Tcp>(listen_handle) {
            Ok(protocol) => protocol,
            Err(error) => {
                let _ =
                    unsafe { (binding.0.destroy_child)(&mut binding.0, listen_handle.as_ptr()) };
                return Err(error);
            }
        };
        let event = match completion_event() {
            Ok(event) => event,
            Err(error) => {
                drop(protocol);
                let _ =
                    unsafe { (binding.0.destroy_child)(&mut binding.0, listen_handle.as_ptr()) };
                return Err(error);
            }
        };
        let mut listener = Self {
            binding,
            listen_handle,
            protocol: Some(protocol),
            token: Box::new(Tcp4ListenToken {
                completion_token: status_token(&event),
                new_child_handle: ptr::null_mut(),
            }),
            event: Some(event),
        };
        let config = Tcp4ConfigData {
            type_of_service: 0,
            time_to_live: 64,
            access_point: Tcp4AccessPoint {
                use_default_address: true.into(),
                station_address: Ipv4Address([0; 4]),
                subnet_mask: Ipv4Address([0; 4]),
                station_port: 2999,
                remote_address: Ipv4Address([0; 4]),
                remote_port: 0,
                active_flag: false.into(),
            },
            control_option: ptr::null_mut(),
        };
        let tcp = listener.protocol.as_mut().expect("open TCP protocol");
        unsafe { (tcp.0.configure)(&mut tcp.0, &config) }.to_result()?;
        listener.arm()?;
        Ok(listener)
    }

    fn arm(&mut self) -> uefi::Result<()> {
        self.token.completion_token.status = Status::NOT_READY;
        self.token.new_child_handle = ptr::null_mut();
        let tcp = self.protocol.as_mut().expect("open TCP protocol");
        // SAFETY: The boxed token and its event remain live until completion
        // or cancellation in Drop.
        unsafe { (tcp.0.accept)(&mut tcp.0, &mut *self.token) }.to_result()
    }

    /// Drive one accepted connection, then return the requested firmware action.
    pub fn poll(
        &mut self,
        ota: &mut Option<OtaController>,
        boot_server: &mut BootServer,
        progress: &mut impl FnMut(),
    ) -> Action {
        let tcp = self.protocol.as_mut().expect("open TCP protocol");
        // SAFETY: TCP polling only touches the configured passive instance.
        let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
        let status = self.token.completion_token.status;
        if status == Status::NOT_READY {
            return Action::None;
        }
        let mut action = Action::None;
        if status == Status::SUCCESS {
            if let Some(handle) = unsafe { Handle::from_ptr(self.token.new_child_handle) } {
                match Connection::new(handle, &mut self.binding, progress) {
                    Ok(mut connection) => {
                        action = connection
                            .handle_http(ota, boot_server)
                            .unwrap_or_else(|error| {
                                crate::logln!("loader_http_error: {error:?}");
                                Action::None
                            });
                        connection.close();
                    }
                    Err(error) => crate::logln!("ota_tcp_child_error: {error:?}"),
                }
            }
        } else {
            crate::logln!("ota_tcp_accept_error: {status:?}");
        }
        if let Err(error) = self.arm() {
            crate::logln!("ota_tcp_rearm_error: {error:?}");
        }
        action
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(mut tcp) = self.protocol.take() {
            if self.token.completion_token.status == Status::NOT_READY {
                // SAFETY: Cancel completes the queued boxed token before it is freed.
                let _ = unsafe { (tcp.0.cancel)(&mut tcp.0, &mut self.token.completion_token) };
                while completion_status(&self.token.completion_token.status) == Status::NOT_READY {
                    let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
                }
            }
            let _ = unsafe { (tcp.0.configure)(&mut tcp.0, ptr::null()) };
            drop(tcp);
        }
        if let Some(event) = self.event.take() {
            let _ = boot::close_event(event);
        }
        // SAFETY: The listener has no live protocol guard or completion token.
        let _ = unsafe {
            (self.binding.0.destroy_child)(&mut self.binding.0, self.listen_handle.as_ptr())
        };
    }
}

#[repr(C)]
struct RxPacket {
    header: Tcp4ReceiveData,
    fragment: Tcp4FragmentData,
}

#[repr(C)]
struct TxPacket {
    header: Tcp4TransmitData,
    fragment: Tcp4FragmentData,
}

struct Connection<'a> {
    child: Handle,
    protocol: Option<ScopedProtocol<Tcp>>,
    binding: &'a mut ScopedProtocol<TcpBinding>,
    progress: &'a mut dyn FnMut(),
}

impl<'a> Connection<'a> {
    fn new(
        child: Handle,
        binding: &'a mut ScopedProtocol<TcpBinding>,
        progress: &'a mut dyn FnMut(),
    ) -> uefi::Result<Self> {
        let protocol = match open_protocol::<Tcp>(child) {
            Ok(protocol) => protocol,
            Err(error) => {
                let _ = unsafe { (binding.0.destroy_child)(&mut binding.0, child.as_ptr()) };
                return Err(error);
            }
        };
        Ok(Self {
            child,
            protocol: Some(protocol),
            binding,
            progress,
        })
    }

    fn wait(&mut self, token: &mut Tcp4IoToken) -> uefi::Result<()> {
        let tcp = self.protocol.as_mut().expect("open TCP protocol");
        for _ in 0..IO_WAIT_POLLS {
            if token.completion_token.status != Status::NOT_READY {
                return token.completion_token.status.to_result();
            }
            let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
            if token.completion_token.status != Status::NOT_READY {
                return token.completion_token.status.to_result();
            }
            (self.progress)();
            boot::stall(core::time::Duration::from_millis(10));
        }
        // SAFETY: Wait until Cancel has completed before stack-backed token
        // and fragment buffers are released.
        unsafe { (tcp.0.cancel)(&mut tcp.0, &mut token.completion_token) }.to_result()?;
        while completion_status(&token.completion_token.status) == Status::NOT_READY {
            let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
        }
        Err(Status::TIMEOUT.into())
    }

    fn receive(&mut self, buf: &mut [u8]) -> uefi::Result<usize> {
        let event = completion_event()?;
        let mut packet = RxPacket {
            header: Tcp4ReceiveData {
                urgent: false.into(),
                data_length: buf.len() as u32,
                fragment_count: 1,
                fragment_table: [],
            },
            fragment: Tcp4FragmentData {
                fragment_length: buf.len() as u32,
                fragment_buf: buf.as_mut_ptr(),
            },
        };
        let mut token = Tcp4IoToken {
            completion_token: status_token(&event),
            packet: Tcp4Packet {
                rx_data: &mut packet.header,
            },
        };
        let tcp = self.protocol.as_mut().expect("open TCP protocol");
        let started = unsafe { (tcp.0.receive)(&mut tcp.0, &mut token) }.to_result();
        let result = started.and_then(|()| {
            self.wait(&mut token)
                .map(|()| packet.header.data_length as usize)
        });
        let _ = boot::close_event(event);
        result
    }

    fn send(&mut self, buf: &[u8]) -> uefi::Result<()> {
        let event = completion_event()?;
        let mut packet = TxPacket {
            header: Tcp4TransmitData {
                push: true.into(),
                urgent: false.into(),
                data_length: buf.len() as u32,
                fragment_count: 1,
                fragment_table: [],
            },
            // SAFETY: UEFI TCP Transmit reads the buffer until token completion.
            fragment: Tcp4FragmentData {
                fragment_length: buf.len() as u32,
                fragment_buf: buf.as_ptr() as *mut u8,
            },
        };
        let mut token = Tcp4IoToken {
            completion_token: status_token(&event),
            packet: Tcp4Packet {
                tx_data: &mut packet.header,
            },
        };
        let tcp = self.protocol.as_mut().expect("open TCP protocol");
        let started = unsafe { (tcp.0.transmit)(&mut tcp.0, &mut token) }.to_result();
        let result = started.and_then(|()| self.wait(&mut token));
        let _ = boot::close_event(event);
        result
    }

    fn close(&mut self) {
        let Some(tcp) = self.protocol.as_mut() else {
            return;
        };
        let Ok(event) = completion_event() else {
            return;
        };
        let mut token = uefi_raw::protocol::network::tcp4::Tcp4CloseToken {
            completion_token: status_token(&event),
            abort_on_close: false.into(),
        };
        if unsafe { (tcp.0.close)(&mut tcp.0, &mut token) } == Status::SUCCESS {
            for _ in 0..IO_WAIT_POLLS {
                if token.completion_token.status != Status::NOT_READY {
                    break;
                }
                let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
                boot::stall(core::time::Duration::from_millis(10));
            }
            if token.completion_token.status == Status::NOT_READY {
                let _ = unsafe { (tcp.0.cancel)(&mut tcp.0, &mut token.completion_token) };
                while completion_status(&token.completion_token.status) == Status::NOT_READY {
                    let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
                }
            }
        }
        let _ = boot::close_event(event);
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Some(mut tcp) = self.protocol.take() {
            let _ = unsafe { (tcp.0.cancel)(&mut tcp.0, ptr::null_mut()) };
            drop(tcp);
        }
        // SAFETY: No token or protocol guard refers to this accepted child.
        let _ = unsafe { (self.binding.0.destroy_child)(&mut self.binding.0, self.child.as_ptr()) };
    }
}

fn completion_status(status: &Status) -> Status {
    // SAFETY: the live token owns this aligned field. Firmware may update it
    // during Poll/Cancel; a volatile load observes each completion before reuse.
    unsafe { ptr::read_volatile(status) }
}

impl HttpConnection for Connection<'_> {
    fn receive(&mut self, buf: &mut [u8]) -> uefi::Result<usize> {
        Connection::receive(self, buf)
    }

    fn send(&mut self, buf: &[u8]) -> uefi::Result<()> {
        Connection::send(self, buf)
    }
}
