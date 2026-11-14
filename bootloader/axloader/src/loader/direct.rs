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
    armed: bool,
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
            armed: false,
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
        let result = unsafe { (tcp.0.accept)(&mut tcp.0, &mut *self.token) }.to_result();
        self.armed = result.is_ok();
        result
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
        // SAFETY: The boxed listener token is initialized, aligned and remains
        // at a stable address; firmware owns its status during Accept.
        let status = unsafe { completion_status(&raw const self.token.completion_token.status) };
        if status == Status::NOT_READY {
            return Action::None;
        }
        self.armed = false;
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
                        let booting = matches!(&action, Action::Boot(_));
                        if booting {
                            crate::logln!("handoff_stage: http_close_start");
                        }
                        connection.close();
                        if booting {
                            crate::logln!("handoff_stage: http_closed");
                        }
                    }
                    Err(error) => crate::logln!("ota_tcp_child_error: {error:?}"),
                }
            }
        } else {
            crate::logln!("ota_tcp_accept_error: {status:?}");
        }
        if !matches!(&action, Action::Boot(_))
            && let Err(error) = self.arm()
        {
            crate::logln!("ota_tcp_rearm_error: {error:?}");
        }
        action
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(mut tcp) = self.protocol.take() {
            let listener_token_queued = self.armed;
            if listener_token_queued {
                self.armed = false;
                // SAFETY: Cancel completes the queued boxed token before it is freed.
                let cancel =
                    unsafe { (tcp.0.cancel)(&mut tcp.0, &mut self.token.completion_token) };
                if cancel != Status::SUCCESS && cancel != Status::NOT_FOUND {
                    crate::logln!("loader_tcp_listen_cancel_failed: {cancel:?}");
                }
                let status = &raw const self.token.completion_token.status;
                // SAFETY: The boxed listener token remains at a stable address
                // and permits firmware updates until this drop completes.
                if !unsafe { wait_for_completion(&mut tcp, status) } {
                    crate::logln!("loader_tcp_listen_cancel_timeout");
                }
            }
            abort_tcp(&mut tcp, "loader_tcp_listen");
            // SAFETY: Configure(NULL) has completed supported async tokens and
            // the boxed listener token remains live until this drop returns.
            if listener_token_queued
                && unsafe { completion_status(&raw const self.token.completion_token.status) }
                    == Status::NOT_READY
            {
                crate::logln!("loader_tcp_listen_reset_incomplete");
                uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::DEVICE_ERROR, None);
            }
            drop(tcp);
        }
        if let Some(event) = self.event.take()
            && let Err(error) = boot::close_event(event)
        {
            crate::logln!("loader_tcp_listen_event_close_failed: {error:?}");
        }
        // SAFETY: The listener has no live protocol guard or completion token.
        let destroy = unsafe {
            (self.binding.0.destroy_child)(&mut self.binding.0, self.listen_handle.as_ptr())
        };
        if destroy != Status::SUCCESS {
            crate::logln!("loader_tcp_listen_destroy_failed: {destroy:?}");
            uefi::runtime::reset(uefi::runtime::ResetType::COLD, destroy, None);
        }
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
        let status = &raw const token.completion_token.status;
        for _ in 0..IO_WAIT_POLLS {
            // SAFETY: The stack token lives through this call and firmware may
            // update its aligned status while the I/O operation is pending.
            let current = unsafe { completion_status(status) };
            if current != Status::NOT_READY {
                return current.to_result();
            }
            let _ = unsafe { (tcp.0.poll)(&mut tcp.0) };
            // SAFETY: The token remains live at the same address across Poll.
            let current = unsafe { completion_status(status) };
            if current != Status::NOT_READY {
                return current.to_result();
            }
            (self.progress)();
            boot::stall(core::time::Duration::from_millis(10));
        }
        // SAFETY: Wait until Cancel has completed before stack-backed token
        // and fragment buffers are released. A firmware that cannot complete
        // the token is reset instead of freeing memory it may still reference.
        let cancel = unsafe { (tcp.0.cancel)(&mut tcp.0, &mut token.completion_token) };
        if cancel != Status::SUCCESS && cancel != Status::NOT_FOUND {
            crate::logln!("loader_tcp_io_cancel_failed: {cancel:?}");
        }
        let status = &raw const token.completion_token.status;
        // SAFETY: Cancel has returned, but the live stack token may still be
        // updated asynchronously. Poll until completion before releasing it.
        if !unsafe { wait_for_completion(tcp, status) } {
            crate::logln!("loader_tcp_io_cancel_timeout");
            abort_tcp(tcp, "loader_tcp_io");
            // SAFETY: The token remains live until this function returns.
            if unsafe { completion_status(status) } == Status::NOT_READY {
                crate::logln!("loader_tcp_io_reset_incomplete");
                uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::DEVICE_ERROR, None);
            }
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
        let event = match completion_event() {
            Ok(event) => event,
            Err(error) => {
                let error = error.status();
                crate::logln!("loader_tcp_close_event_failed: {error:?}");
                abort_tcp(tcp, "loader_tcp_close");
                return;
            }
        };
        let mut token = uefi_raw::protocol::network::tcp4::Tcp4CloseToken {
            completion_token: status_token(&event),
            abort_on_close: false.into(),
        };
        let close = unsafe { (tcp.0.close)(&mut tcp.0, &mut token) };
        if close != Status::SUCCESS {
            crate::logln!("loader_tcp_close_failed: {close:?}");
            abort_tcp(tcp, "loader_tcp_close");
        } else {
            let status = &raw const token.completion_token.status;
            // SAFETY: The stack token and event remain live and at stable
            // addresses throughout Close completion polling.
            if !unsafe { wait_for_completion(tcp, status) } {
                crate::logln!("loader_tcp_close_timeout");
                abort_tcp(tcp, "loader_tcp_close");
                // SAFETY: Configure(NULL) completed Close tokens before this read.
                if unsafe { completion_status(status) } == Status::NOT_READY {
                    crate::logln!("loader_tcp_close_reset_incomplete");
                    uefi::runtime::reset(
                        uefi::runtime::ResetType::COLD,
                        Status::DEVICE_ERROR,
                        None,
                    );
                }
                // SAFETY: The token is still live and this is its final status read.
                let final_status = unsafe { completion_status(status) };
                if final_status != Status::SUCCESS {
                    crate::logln!("loader_tcp_close_aborted: {final_status:?}");
                }
            }
        }
        if let Err(error) = boot::close_event(event) {
            crate::logln!("loader_tcp_close_event_close_failed: {error:?}");
        }
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Some(mut tcp) = self.protocol.take() {
            let _ = unsafe { (tcp.0.cancel)(&mut tcp.0, ptr::null_mut()) };
            drop(tcp);
        }
        // SAFETY: No token or protocol guard refers to this accepted child.
        let destroy =
            unsafe { (self.binding.0.destroy_child)(&mut self.binding.0, self.child.as_ptr()) };
        if destroy != Status::SUCCESS {
            crate::logln!("loader_tcp_connection_destroy_failed: {destroy:?}");
            uefi::runtime::reset(uefi::runtime::ResetType::COLD, destroy, None);
        }
    }
}

/// # Safety
///
/// `status` must point to an initialized, aligned status field in a token that
/// remains alive at the same address throughout this call. The token must
/// permit firmware to update that field while TCP is polled.
unsafe fn wait_for_completion(tcp: &mut Tcp, status: *const Status) -> bool {
    for _ in 0..IO_WAIT_POLLS {
        // SAFETY: The caller guarantees a live, firmware-updatable token field.
        if unsafe { completion_status(status) } != Status::NOT_READY {
            return true;
        }
        let poll = unsafe { (tcp.0.poll)(&mut tcp.0) };
        if poll != Status::SUCCESS && poll != Status::NOT_READY {
            crate::logln!("loader_tcp_poll_failed: {poll:?}");
            break;
        }
        boot::stall(core::time::Duration::from_millis(10));
    }
    // SAFETY: The caller guarantees the token remains live for this final read.
    (unsafe { completion_status(status) }) != Status::NOT_READY
}

fn abort_tcp(tcp: &mut Tcp, context: &'static str) {
    // Tcp4 Configure(NULL) is the specified reset operation. Unlike Cancel,
    // it also aborts a pending Close and completes all supported async tokens.
    let reset = unsafe { (tcp.0.configure)(&mut tcp.0, ptr::null()) };
    if reset != Status::SUCCESS {
        crate::logln!("{context}_reset_failed: {reset:?}");
        uefi::runtime::reset(uefi::runtime::ResetType::COLD, reset, None);
    }
}

/// # Safety
///
/// `status` must point to an initialized, aligned status field in a live token.
/// The token must permit firmware updates during the volatile read.
unsafe fn completion_status(status: *const Status) -> Status {
    // SAFETY: The caller guarantees the token field remains live and aligned.
    // Volatile access observes firmware changes without borrowing the field.
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
