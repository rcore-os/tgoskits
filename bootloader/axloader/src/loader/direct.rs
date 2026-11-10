//! Bounded, single-connection UEFI TCP4 HTTP control endpoint.

extern crate alloc;

use alloc::{boxed::Box, format, vec::Vec};
use core::ptr;

use axloader::{
    boot_job::operation_error,
    http_control::{
        MAX_HEADER_BYTES, Route, commit_after_successful_write, error_status, operation_error_json,
        parse_head, qemu_202_write_failure, route, validate_body_length, validate_request_protocol,
    },
    integrity::decode_sha256,
    ota::OtaController,
    target::CURRENT,
};
use httpboot_protocol::{
    DEVICE_PROTOCOL_VERSION, DeviceBootJob, DeviceOperationError, LoaderDeviceStatus,
    MAX_HTTP_BOOT_INITRAMFS_BYTES, OtaSource,
};
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

use super::boot_server::{BootExecution, BootServer};

const TCP4_BINDING_GUID: uefi::Guid = uefi::guid!("00720665-67eb-4a99-baf7-d3c33a1c7cc9");
const TCP4_GUID: uefi::Guid = uefi::guid!("65530bc7-a359-410f-b010-5aadc7ec2b62");
const IO_WAIT_POLLS: usize = 3000;

fn injected_202_write_error() -> uefi::Error {
    Status::ABORTED.into()
}

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

pub enum Action {
    None,
    Reset,
    Boot(alloc::boxed::Box<BootExecution>),
}

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
        let status = self.token.completion_token.status;
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
                if !wait_for_completion(&mut tcp, &self.token.completion_token.status) {
                    crate::logln!("loader_tcp_listen_cancel_timeout");
                }
            }
            abort_tcp(&mut tcp, "loader_tcp_listen");
            if listener_token_queued
                && completion_status(&self.token.completion_token.status) == Status::NOT_READY
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
        // and fragment buffers are released. A firmware that cannot complete
        // the token is reset instead of freeing memory it may still reference.
        let cancel = unsafe { (tcp.0.cancel)(&mut tcp.0, &mut token.completion_token) };
        if cancel != Status::SUCCESS && cancel != Status::NOT_FOUND {
            crate::logln!("loader_tcp_io_cancel_failed: {cancel:?}");
        }
        if !wait_for_completion(tcp, &token.completion_token.status) {
            crate::logln!("loader_tcp_io_cancel_timeout");
            abort_tcp(tcp, "loader_tcp_io");
            if completion_status(&token.completion_token.status) == Status::NOT_READY {
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

    fn reply(&mut self, status: &str, body: &[u8]) -> uefi::Result<()> {
        let headers = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
             {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        self.send(headers.as_bytes())?;
        if !body.is_empty() {
            self.send(body)?;
        }
        Ok(())
    }

    fn reply_error(&mut self, error: &DeviceOperationError) -> uefi::Result<()> {
        let body = operation_error_json(error).map_err(|_| Status::ABORTED)?;
        self.reply(error_status(&error.code), &body)
    }

    fn read_body(&mut self, length: usize, initial: &[u8]) -> uefi::Result<Vec<u8>> {
        if initial.len() > length {
            return Err(Status::BAD_BUFFER_SIZE.into());
        }
        let mut body = Vec::new();
        body.try_reserve_exact(length)
            .map_err(|_| Status::OUT_OF_RESOURCES)?;
        body.extend_from_slice(initial);
        let mut chunk = [0; 8192];
        while body.len() < length {
            let limit = (length - body.len()).min(chunk.len());
            let count = self.receive(&mut chunk[..limit])?;
            if count == 0 {
                return Err(Status::END_OF_FILE.into());
            }
            body.extend_from_slice(&chunk[..count]);
        }
        Ok(body)
    }

    fn handle_http(
        &mut self,
        ota: &mut Option<OtaController>,
        boot_server: &mut BootServer,
    ) -> uefi::Result<Action> {
        let mut header = [0; MAX_HEADER_BYTES + 1];
        let mut used = 0;
        let header_end = loop {
            if used >= MAX_HEADER_BYTES {
                self.reply_error(&operation_error(
                    "request_header_too_large",
                    "HTTP request header exceeds the control-plane limit",
                ))?;
                return Ok(Action::None);
            }
            let count = self.receive(&mut header[used..])?;
            if count == 0 {
                return Ok(Action::None);
            }
            used += count;
            if let Some(index) = header[..used]
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
            {
                if index + 4 > MAX_HEADER_BYTES {
                    self.reply_error(&operation_error(
                        "request_header_too_large",
                        "HTTP request header exceeds the control-plane limit",
                    ))?;
                    return Ok(Action::None);
                }
                break index + 4;
            }
        };
        let (head, parsed_header_end) = match parse_head(&header[..used]) {
            Ok(value) => value,
            Err(error) => {
                self.reply_error(&operation_error("invalid_request", error))?;
                return Ok(Action::None);
            }
        };
        debug_assert_eq!(parsed_header_end, header_end);
        if let Err(error) = validate_request_protocol(&head) {
            self.reply_error(&error)?;
            return Ok(Action::None);
        }
        let method = head.method.as_str();
        let path = head.path.as_str();
        let selected_route = route(method, path);
        let content_length = Some(head.content_length);
        let initial = &header[header_end..used];
        if let Err(error) = selected_route.check_admission(
            head.header("x-boot-epoch"),
            boot_server.epoch(),
            ota.as_ref().is_some_and(OtaController::trial),
        ) {
            self.reply(error.status(), error.body())?;
            return Ok(Action::None);
        }
        let upload_size = if let Route::BootJobUpload { id, kind } = selected_route {
            let descriptor = match boot_server.descriptor(id, kind) {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    self.reply_error(&error)?;
                    return Ok(Action::None);
                }
            };
            let expected_size = descriptor.size;
            let expected_digest = decode_sha256(&descriptor.sha256);
            if content_length != Some(expected_size as usize) {
                let error = operation_error(
                    "image_size_mismatch",
                    format!(
                        "upload length {:?} does not match declared length {}",
                        content_length, expected_size
                    ),
                );
                let _ = boot_server.fail(id, error.clone());
                self.reply_error(&error)?;
                return Ok(Action::None);
            }
            if head.ota_image_sha256() != expected_digest {
                let error = operation_error(
                    "image_digest_mismatch",
                    "X-Image-Sha256 does not match the boot manifest",
                );
                let _ = boot_server.fail(id, error.clone());
                self.reply_error(&error)?;
                return Ok(Action::None);
            }
            Some(expected_size as usize)
        } else {
            None
        };
        if let Route::BootJobStart { id } = selected_route {
            match boot_server.failure(id) {
                Ok(Some(error)) => {
                    self.reply_error(error)?;
                    return Ok(Action::None);
                }
                Err(error) => {
                    self.reply_error(&error)?;
                    return Ok(Action::None);
                }
                Ok(None) => {}
            }
        }
        if let (Route::BootJobUpload { id, .. }, Some(expected_size)) =
            (selected_route, upload_size)
            && initial.len() > expected_size
        {
            let error = operation_error(
                "image_size_mismatch",
                format!(
                    "upload exceeded declared length {expected_size}: received at least {}",
                    initial.len()
                ),
            );
            let _ = boot_server.fail(id, error.clone());
            self.reply_error(&error)?;
            return Ok(Action::None);
        }
        if let Err(error) = validate_body_length(
            method,
            path,
            head.content_length,
            initial.len(),
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
        ) {
            self.reply_error(&operation_error(error, error))?;
            return Ok(Action::None);
        }
        if selected_route == Route::DeviceStatus {
            let body = serde_json::to_vec(&LoaderDeviceStatus {
                protocol_version: DEVICE_PROTOCOL_VERSION,
                boot_epoch: boot_server.epoch().into(),
                mac_address: boot_server.mac_address(),
                current_mac_address: boot_server.current_mac_address(),
                arch: CURRENT.arch_name.into(),
                loader_version: env!("CARGO_PKG_VERSION").into(),
                hardware: boot_server.hardware().clone(),
                boot: boot_server.status(),
                serial: Some(boot_server.serial.borrow().clone()),
                ota: ota.as_ref().map(OtaController::protocol_state),
            })
            .map_err(|_| Status::ABORTED)?;
            self.reply("200 OK", &body)?;
            return Ok(Action::None);
        }
        if selected_route == Route::OtaStatus {
            let Some(ota) = ota.as_ref() else {
                self.reply_error(&operation_error(
                    "ota_unavailable",
                    "OTA control is unavailable from this loader",
                ))?;
                return Ok(Action::None);
            };
            self.reply(
                "200 OK",
                &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if let Route::BootJobStatus { id } = selected_route {
            match boot_server.status_for(id) {
                Ok(status) => self.reply(
                    "200 OK",
                    &serde_json::to_vec(&status).map_err(|_| Status::ABORTED)?,
                )?,
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if selected_route == Route::SerialContinue {
            let Some(length) = content_length.filter(|n| *n > 0 && *n <= 1024) else {
                self.reply_error(&operation_error(
                    "invalid_serial_binding",
                    "serial binding body must be 1-1024 bytes",
                ))?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let binding = match serde_json::from_slice::<httpboot_protocol::SerialBinding>(&body) {
                Ok(binding) => binding,
                Err(_) => {
                    self.reply_error(&operation_error(
                        "invalid_serial_binding",
                        "serial binding JSON is invalid",
                    ))?;
                    return Ok(Action::None);
                }
            };
            let result = boot_server.serial.borrow_mut().grant(binding);
            let status =
                serde_json::to_vec(&*boot_server.serial.borrow()).map_err(|_| Status::ABORTED)?;
            if result.is_ok() {
                self.reply("200 OK", &status)?;
            } else {
                self.reply_error(&operation_error(
                    "serial_binding_conflict",
                    "serial binding is already owned by another request",
                ))?;
            }
            return Ok(Action::None);
        }
        if let Route::SerialBindingRevoke { id } = selected_route {
            let result = boot_server.serial.borrow_mut().revoke(id);
            if result.is_ok() {
                self.reply("200 OK", b"{}")?;
            } else {
                self.reply_error(&operation_error(
                    "serial_binding_conflict",
                    "serial binding does not match the active owner",
                ))?;
            }
            return Ok(Action::None);
        }
        if let Some(ota) = ota.as_mut() {
            if selected_route == Route::OtaConfirm {
                let Some(length) = content_length.filter(|size| *size > 0 && *size <= 1024) else {
                    self.reply_error(&operation_error(
                        "invalid_ota_confirmation",
                        "OTA confirmation body must be 1-1024 bytes",
                    ))?;
                    return Ok(Action::None);
                };
                let body = self.read_body(length, initial)?;
                let value: serde_json::Value = match serde_json::from_slice(&body) {
                    Ok(value) => value,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_confirmation",
                            "OTA confirmation JSON is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                let id = value
                    .get("update_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty());
                let Some(id) = id else {
                    self.reply_error(&operation_error(
                        "invalid_ota_confirmation",
                        "OTA confirmation requires a non-empty update_id",
                    ))?;
                    return Ok(Action::None);
                };
                let source = match head.ota_source() {
                    Ok(source) => source,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_source",
                            "OTA update source is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                if ota.confirm(id, source).is_err() {
                    self.reply_error(&operation_error(
                        "ota_confirmation_conflict",
                        "OTA update ID or source does not match the pending trial",
                    ))?;
                } else {
                    self.reply(
                        "200 OK",
                        &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                return Ok(Action::None);
            }
            if selected_route == Route::OtaImage {
                if boot_server.busy() {
                    self.reply_error(&operation_error(
                        "boot_job_busy",
                        "a boot job is active during OTA upload",
                    ))?;
                    return Ok(Action::None);
                }
                let Some(length) = content_length
                    .filter(|size| *size > 0 && *size <= axloader::ota::MAX_IMAGE_BYTES)
                else {
                    self.reply_error(&operation_error(
                        "image_too_large",
                        "OTA image is empty or exceeds the device limit",
                    ))?;
                    return Ok(Action::None);
                };
                let Some(expected) = head.ota_image_sha256() else {
                    self.reply_error(&operation_error(
                        "invalid_ota_image",
                        "X-Image-Sha256 is missing or invalid",
                    ))?;
                    return Ok(Action::None);
                };
                if initial.len() > length {
                    self.reply_error(&operation_error(
                        "invalid_request",
                        "request body exceeds Content-Length",
                    ))?;
                    return Ok(Action::None);
                }
                let source = match head.ota_source() {
                    Ok(source) => source,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_source",
                            "OTA update source is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                let display_version = match head.ota_image_version() {
                    Ok(version) => version,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_image_version",
                            "OTA image version is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                let id = if source == OtaSource::Server {
                    let Some(id) = head.ota_update_id() else {
                        self.reply_error(&operation_error(
                            "invalid_ota_update_id",
                            "server OTA upload requires a valid X-Update-ID",
                        ))?;
                        return Ok(Action::None);
                    };
                    id
                } else {
                    ota.next_direct_id(&expected)
                };
                let Ok((disk, mut writer)) = ota.start_update(length) else {
                    self.reply_error(&operation_error(
                        "ota_state_conflict",
                        "OTA storage is not ready for another update",
                    ))?;
                    return Ok(Action::None);
                };
                writer
                    .write(initial)
                    .inspect_err(|_| ota.record_failure("image_write_failed"))?;
                let mut written = initial.len();
                let mut chunk = [0; 8192];
                while written < length {
                    let limit = (length - written).min(chunk.len());
                    let count = self
                        .receive(&mut chunk[..limit])
                        .inspect_err(|_| ota.record_failure("image_receive_failed"))?;
                    if count == 0 {
                        ota.record_failure("short_image_upload");
                        self.reply_error(&operation_error(
                            "image_receive_failed",
                            "OTA upload ended before Content-Length bytes",
                        ))?;
                        return Ok(Action::None);
                    }
                    writer
                        .write(&chunk[..count])
                        .inspect_err(|_| ota.record_failure("image_write_failed"))?;
                    written += count;
                }
                if ota
                    .finish_update(disk, writer, expected, id, source, display_version)
                    .is_err()
                {
                    ota.record_failure("image_digest_or_efi_validation_failed");
                    self.reply_error(&operation_error(
                        "invalid_ota_image",
                        "OTA image digest, checksum or EFI structure is invalid",
                    ))?;
                    return Ok(Action::None);
                }
                self.reply(
                    "202 Accepted",
                    &serde_json::to_vec(&serde_json::json!({
                        "update_id": core::str::from_utf8(&id).unwrap_or("")
                    }))
                    .map_err(|_| Status::ABORTED)?,
                )?;
                return Ok(Action::Reset);
            }
        } else if path.starts_with("/api/v1/ota/") {
            self.reply_error(&operation_error(
                "ota_unavailable",
                "OTA control is unavailable from this loader",
            ))?;
            return Ok(Action::None);
        }
        if selected_route == Route::BootJobCreate {
            let Some(length) = content_length.filter(|size| *size > 0 && *size <= MAX_HEADER_BYTES)
            else {
                self.reply_error(&operation_error(
                    "invalid_boot_manifest",
                    "boot manifest body is empty or too large",
                ))?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let manifest: DeviceBootJob = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => {
                    self.reply_error(&operation_error(
                        "invalid_boot_manifest",
                        "boot manifest JSON does not match the v7 schema",
                    ))?;
                    return Ok(Action::None);
                }
            };
            let boot_id = manifest.boot_id.clone();
            match boot_server.create(manifest) {
                Ok(created) => {
                    let status = boot_server
                        .status_for(&boot_id)
                        .map_err(|_| Status::ABORTED)?;
                    self.reply(
                        if created { "201 Created" } else { "200 OK" },
                        &serde_json::to_vec(&status).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if let Route::BootJobCancel { id } = selected_route {
            match boot_server.cancel(id) {
                Ok(()) => self.reply("204 No Content", b"")?,
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if let Route::BootJobUpload { id, kind } = selected_route {
            let expected_size = upload_size.expect("upload preflight produced a descriptor size");
            let data = match self.read_body(expected_size, initial) {
                Ok(data) => data,
                Err(error) => {
                    let status = error.status();
                    if matches!(status, Status::END_OF_FILE | Status::BAD_BUFFER_SIZE) {
                        let reason = if status == Status::END_OF_FILE {
                            format!("upload ended before declared length {expected_size}")
                        } else {
                            format!("upload exceeded declared length {expected_size}")
                        };
                        let error = operation_error("image_size_mismatch", reason);
                        let _ = boot_server.fail(id, error.clone());
                        self.reply_error(&error)?;
                        return Ok(Action::None);
                    }
                    let _ = boot_server.fail(
                        id,
                        operation_error(
                            "image_receive_failed",
                            format!("failed to receive complete upload: {error:?}"),
                        ),
                    );
                    return Err(error);
                }
            };
            match boot_server.upload(id, kind, data) {
                Ok(()) => {
                    let status = boot_server.status_for(id).map_err(|_| Status::ABORTED)?;
                    self.reply(
                        "200 OK",
                        &serde_json::to_vec(&status).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if let Route::BootJobStart { id } = selected_route {
            match boot_server.prepare(id, head.header("x-serial-binding")) {
                Ok(execution) => {
                    let action = Action::Boot(Box::new(execution));
                    let injected_error =
                        qemu_202_write_failure(id).then_some(injected_202_write_error);
                    return commit_after_successful_write(
                        || self.reply("202 Accepted", br#"{"phase":"ready_to_handoff"}"#),
                        action,
                        injected_error,
                    );
                }
                Err(error) => {
                    self.reply_error(&error)?;
                    return Ok(Action::None);
                }
            }
        }
        self.reply_error(&operation_error(
            "not_found",
            "control endpoint does not exist",
        ))?;
        Ok(Action::None)
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
        } else if !wait_for_completion(tcp, &token.completion_token.status) {
            crate::logln!("loader_tcp_close_timeout");
            abort_tcp(tcp, "loader_tcp_close");
            if completion_status(&token.completion_token.status) == Status::NOT_READY {
                crate::logln!("loader_tcp_close_reset_incomplete");
                uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::DEVICE_ERROR, None);
            }
            let status = completion_status(&token.completion_token.status);
            if status != Status::SUCCESS {
                crate::logln!("loader_tcp_close_aborted: {status:?}");
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

fn wait_for_completion(tcp: &mut Tcp, status: &Status) -> bool {
    for _ in 0..IO_WAIT_POLLS {
        if completion_status(status) != Status::NOT_READY {
            return true;
        }
        let poll = unsafe { (tcp.0.poll)(&mut tcp.0) };
        if poll != Status::SUCCESS && poll != Status::NOT_READY {
            crate::logln!("loader_tcp_poll_failed: {poll:?}");
            break;
        }
        boot::stall(core::time::Duration::from_millis(10));
    }
    completion_status(status) != Status::NOT_READY
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

fn completion_status(status: &Status) -> Status {
    // SAFETY: the live token owns this aligned field. Firmware may update it
    // during Poll/Cancel; a volatile load observes each completion before reuse.
    unsafe { ptr::read_volatile(status) }
}
