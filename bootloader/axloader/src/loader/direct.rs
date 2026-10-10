//! Bounded, single-connection UEFI TCP4 HTTP control endpoint.

extern crate alloc;

use alloc::{boxed::Box, format, vec::Vec};
use core::ptr;

use axloader::{integrity::decode_sha256, ota::OtaController};
use httpboot_protocol::{
    BootArch, DEVICE_PROTOCOL_VERSION, DeviceBootJob, LoaderDeviceStatus, OtaSource,
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

use super::boot_server::{BootExecution, BootServer, FileKind};

const TCP4_BINDING_GUID: uefi::Guid = uefi::guid!("00720665-67eb-4a99-baf7-d3c33a1c7cc9");
const TCP4_GUID: uefi::Guid = uefi::guid!("65530bc7-a359-410f-b010-5aadc7ec2b62");
const MAX_HEADER: usize = 4096;
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
        let mut header = [0; MAX_HEADER + 1];
        let mut used = 0;
        let header_end = loop {
            if used >= MAX_HEADER {
                self.reply("431 Request Header Fields Too Large", b"{}")?;
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
                if index + 4 > MAX_HEADER {
                    self.reply("431 Request Header Fields Too Large", b"{}")?;
                    return Ok(Action::None);
                }
                break index + 4;
            }
        };
        let request =
            core::str::from_utf8(&header[..header_end]).map_err(|_| Status::INVALID_PARAMETER)?;
        let Some(first) = request.split("\r\n").next() else {
            return Ok(Action::None);
        };
        let mut words = first.split_ascii_whitespace();
        let (Some(method), Some(path), Some("HTTP/1.1" | "HTTP/1.0"), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            self.reply("400 Bad Request", b"{}")?;
            return Ok(Action::None);
        };
        let mut content_length = None;
        let mut sha256 = None;
        let mut display_version = None;
        let mut update_id = None;
        let mut source = OtaSource::Direct;
        let mut epoch = None;
        let mut serial_binding = None;
        for line in request.split("\r\n").skip(1) {
            if line.is_empty() {
                break;
            }
            let Some((key, value)) = line.split_once(':') else {
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            };
            let value = value.trim();
            if key.eq_ignore_ascii_case("content-length") {
                if content_length
                    .replace(
                        value
                            .parse::<usize>()
                            .map_err(|_| Status::INVALID_PARAMETER)?,
                    )
                    .is_some()
                {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
            } else if key.eq_ignore_ascii_case("transfer-encoding") {
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            } else if key.eq_ignore_ascii_case("x-image-sha256") {
                sha256 = decode_sha256(value);
            } else if key.eq_ignore_ascii_case("x-image-version") {
                if value.len() > 96 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
                display_version = Some(value);
            } else if key.eq_ignore_ascii_case("x-update-id") {
                update_id = Some(value);
            } else if key.eq_ignore_ascii_case("x-update-source") {
                if value != "server" {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
                source = OtaSource::Server;
            } else if key.eq_ignore_ascii_case("x-serial-binding") {
                serial_binding = Some(value);
            } else if key.eq_ignore_ascii_case("x-boot-epoch") {
                epoch = Some(value);
            }
        }
        let initial = &header[header_end..used];
        if method == "GET" && path == "/api/v1/status" {
            let body = serde_json::to_vec(&LoaderDeviceStatus {
                protocol_version: DEVICE_PROTOCOL_VERSION,
                boot_epoch: boot_server.epoch().into(),
                mac_address: boot_server.mac_address(),
                current_mac_address: boot_server.current_mac_address(),
                arch: BootArch::X86_64,
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
        if method == "GET" && path == "/api/v1/ota/status" {
            let Some(ota) = ota.as_ref() else {
                self.reply("503 Service Unavailable", b"{}")?;
                return Ok(Action::None);
            };
            self.reply(
                "200 OK",
                &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if method == "GET" && path.starts_with("/api/v1/boot/jobs/") {
            let id = &path["/api/v1/boot/jobs/".len()..];
            if boot_server.descriptor(id, FileKind::Kernel).is_err() {
                self.reply("404 Not Found", b"{}")?;
            } else {
                self.reply(
                    "200 OK",
                    &serde_json::to_vec(&boot_server.status()).map_err(|_| Status::ABORTED)?,
                )?;
            }
            return Ok(Action::None);
        }
        if epoch != Some(boot_server.epoch()) {
            self.reply("409 Conflict", br#"{"error":"stale_boot_epoch"}"#)?;
            return Ok(Action::None);
        }
        if method == "POST" && path == "/api/v1/serial/continue" {
            let Some(length) = content_length.filter(|n| *n > 0 && *n <= 1024) else {
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let binding = match serde_json::from_slice::<httpboot_protocol::SerialBinding>(&body) {
                Ok(binding) => binding,
                Err(_) => {
                    self.reply("400 Bad Request", br#"{"error":"invalid_serial_binding"}"#)?;
                    return Ok(Action::None);
                }
            };
            let result = boot_server.serial.borrow_mut().grant(binding);
            let status =
                serde_json::to_vec(&*boot_server.serial.borrow()).map_err(|_| Status::ABORTED)?;
            self.reply(
                if result.is_ok() {
                    "200 OK"
                } else {
                    "409 Conflict"
                },
                &status,
            )?;
            return Ok(Action::None);
        }
        if method == "DELETE"
            && let Some(id) = path.strip_prefix("/api/v1/serial/bindings/")
        {
            let result = boot_server.serial.borrow_mut().revoke(id);
            self.reply(
                if result.is_ok() {
                    "200 OK"
                } else {
                    "409 Conflict"
                },
                b"{}",
            )?;
            return Ok(Action::None);
        }
        if let Some(ota) = ota.as_mut() {
            if method == "POST" && path == "/api/v1/ota/confirm" {
                let Some(length) = content_length.filter(|size| *size > 0 && *size <= 1024) else {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                };
                let body = self.read_body(length, initial)?;
                let value: serde_json::Value =
                    serde_json::from_slice(&body).map_err(|_| Status::INVALID_PARAMETER)?;
                let id = value
                    .get("update_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if ota.confirm(id, source).is_err() {
                    self.reply("409 Conflict", b"{}")?;
                } else {
                    self.reply(
                        "200 OK",
                        &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                return Ok(Action::None);
            }
            if method == "PUT" && path == "/api/v1/ota/image" {
                if boot_server.busy() {
                    self.reply("409 Conflict", br#"{"error":"boot_job_busy"}"#)?;
                    return Ok(Action::None);
                }
                let Some(length) = content_length
                    .filter(|size| *size > 0 && *size <= axloader::ota::MAX_IMAGE_BYTES)
                else {
                    self.reply("413 Payload Too Large", b"{}")?;
                    return Ok(Action::None);
                };
                let Some(expected) = sha256 else {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                };
                if initial.len() > length {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
                let id = if source == OtaSource::Server {
                    let Some(id) = update_id.and_then(|value| value.as_bytes().try_into().ok())
                    else {
                        self.reply("400 Bad Request", b"{}")?;
                        return Ok(Action::None);
                    };
                    id
                } else {
                    ota.next_direct_id(&expected)
                };
                let Ok((disk, mut writer)) = ota.start_update(length) else {
                    self.reply("409 Conflict", b"{}")?;
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
                        self.reply("400 Bad Request", b"{}")?;
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
                    self.reply("422 Unprocessable Content", b"{}")?;
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
            self.reply("503 Service Unavailable", b"{}")?;
            return Ok(Action::None);
        }
        if ota.as_ref().is_some_and(OtaController::trial) {
            self.reply(
                "409 Conflict",
                br#"{"error":"ota_trial_requires_confirmation"}"#,
            )?;
            return Ok(Action::None);
        }
        if method == "POST" && path == "/api/v1/boot/jobs" {
            let Some(length) = content_length.filter(|size| *size > 0 && *size <= MAX_HEADER)
            else {
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let manifest: DeviceBootJob = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
            };
            let status = match boot_server.create(manifest) {
                Ok(true) => "201 Created",
                Ok(false) => "200 OK",
                Err(_) => "409 Conflict",
            };
            self.reply(
                status,
                &serde_json::to_vec(&boot_server.status()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if let Some(tail) = path.strip_prefix("/api/v1/boot/jobs/") {
            let (id, operation) = tail.split_once('/').unwrap_or((tail, ""));
            if method == "DELETE" && operation.is_empty() {
                let status = if boot_server.cancel(id).is_ok() {
                    "204 No Content"
                } else {
                    "404 Not Found"
                };
                self.reply(status, b"")?;
                return Ok(Action::None);
            }
            if method == "PUT" && (operation == "kernel" || operation == "initramfs") {
                let kind = if operation == "kernel" {
                    FileKind::Kernel
                } else {
                    FileKind::Initramfs
                };
                let Ok(descriptor) = boot_server.descriptor(id, kind) else {
                    self.reply("404 Not Found", b"{}")?;
                    return Ok(Action::None);
                };
                if content_length != Some(descriptor.size as usize)
                    || sha256 != decode_sha256(&descriptor.sha256)
                {
                    let _ = boot_server.cancel(id);
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
                let data = match self.read_body(descriptor.size as usize, initial) {
                    Ok(data) => data,
                    Err(error) => {
                        let _ = boot_server.cancel(id);
                        return Err(error);
                    }
                };
                let result = boot_server.upload(id, kind, data);
                self.reply(
                    if result.is_ok() {
                        "200 OK"
                    } else {
                        "422 Unprocessable Content"
                    },
                    &serde_json::to_vec(&boot_server.status()).map_err(|_| Status::ABORTED)?,
                )?;
                return Ok(Action::None);
            }
            if method == "POST" && operation == "start" {
                match boot_server.prepare(id, serial_binding) {
                    Ok(execution) => {
                        self.reply("202 Accepted", br#"{"phase":"ready_to_handoff"}"#)?;
                        return Ok(Action::Boot(alloc::boxed::Box::new(execution)));
                    }
                    Err(error) => {
                        let body = serde_json::to_vec(&serde_json::json!({"error": error}))
                            .map_err(|_| Status::ABORTED)?;
                        self.reply("409 Conflict", &body)?;
                        return Ok(Action::None);
                    }
                }
            }
        }
        self.reply("404 Not Found", b"{}")?;
        Ok(Action::None)
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
