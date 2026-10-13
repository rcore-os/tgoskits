//! LoongArch ArceOS EFI loader frontend.

use core::{ffi::c_void, mem, ptr::NonNull};
use std::{
    format,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket},
    println,
    string::{String, ToString},
    vec::Vec,
};

use axloader::{
    boot_job::{BootJobStore, FileKind},
    integrity,
    ota::{self, Outcome, Slot, State},
    platform::{BootTransport, EfiHandoff, ImageMemory, OtaStorage},
    target::LOONGARCH64_EFI,
};
use httpboot_protocol::{
    BootArch, DEVICE_PROTOCOL_VERSION, DeviceBootJob, LoaderAnnouncement, LoaderDeviceStatus,
    LoaderHardwareInfo, LoaderOtaState, MAX_DISCOVERY_DATAGRAM_BYTES,
    MAX_HTTP_BOOT_INITRAMFS_BYTES, MacAddress, OtaOutcome, OtaSource, SerialParameters,
};
use sha2::{Digest, Sha256};
use uefi::Status;

const HTTP_PORT: u16 = 2999;
const DISCOVERY_PORT: u16 = 2998;
const MAX_HEADER: usize = 4 * 1024;
const MAX_SERIAL_BINDING_BYTES: usize = 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const OTA_ROOT: &str = "axloader";
const PE_MACHINE_LOONGARCH64: u16 = 0x6264;
const PE32_PLUS_MAGIC: u16 = 0x20b;
const PE32_PLUS_OPTIONAL_HEADER_SIZE: usize = 112;
const PE_DATA_DIRECTORY_OFFSET: usize = 112;
const PE_BASE_RELOCATION_DIRECTORY: usize = 5;
const PE_SECTION_HEADER_SIZE: usize = 40;
const PE_SECTION_EXECUTE: u32 = 0x2000_0000;
const ELF_PF_X: u32 = 1;
pub fn run() {
    println!("axloader: LoongArch ArceOS EFI frontend");
    let current_image = current_loader_image()
        .unwrap_or_else(|| panic!("ota_bootstrap: current loader image unavailable"));
    let current_digest: [u8; 32] = Sha256::digest(&current_image).into();
    let mut ota = LaOta::open(&current_image);
    let inherited_slot = someboot::inherited_loader_slot();
    // The parent commits the generation immediately before entering an
    // inherited loader. Treat that handoff as the same boot attempt instead
    // of advancing the durable identity a second time in the child.
    let mut boot_advanced = inherited_slot.is_some();
    if let Some(slot) = inherited_slot {
        ota.set_running_slot(slot)
            .unwrap_or_else(|error| panic!("ota_slot: {error}"));
    } else if let Some(slot) = ota
        .prepare_bootstrap(current_digest)
        .unwrap_or_else(|error| panic!("ota_bootstrap: {error}"))
    {
        // Commit the generation before entering a different slot so the child
        // can reuse this durable identity through the inherited handoff.
        ota.advance_boot()
            .unwrap_or_else(|error| panic!("ota_bootstrap: {error}"));
        boot_advanced = true;
        match ota.launch_slot(slot) {
            Ok(()) => panic!("ota_slot_returned"),
            Err(error) => {
                println!("ota_slot_failed: {error}");
                let _ = ota.rollback(Outcome::LoadFailed);
            }
        }
    }

    if !boot_advanced {
        // Stable boots and firmware-loaded slots without an inherited payload
        // publish an epoch only after committing their new generation.
        ota.advance_boot()
            .unwrap_or_else(|error| panic!("ota_bootstrap: {error}"));
    }
    let interface = select_interface().unwrap_or_else(|error| panic!("network_ready: {error}"));
    let epoch = boot_epoch(interface.mac_address(), ota.boot_identity(current_digest));
    let mut server = BootServer::new(
        epoch,
        interface.mac_address(),
        interface.current_mac_address(),
    );
    server.serial_beacon();
    interface.announce(server.epoch.as_str(), server.serial_ready());
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, HTTP_PORT))
        .unwrap_or_else(|error| panic!("loader_server: {error}"));
    println!(
        "network_ready: arch=loongarch64 mac={} port={HTTP_PORT}",
        format_mac(interface.mac_address())
    );
    println!("loader_server: port={HTTP_PORT}");

    loop {
        server.serial_beacon();
        interface.announce(server.epoch.as_str(), server.serial_ready());
        let (mut stream, _) = listener
            .accept()
            .unwrap_or_else(|error| panic!("loader_accept: {error}"));
        match handle_connection(&mut stream, &mut ota, &mut server) {
            Ok(Action::None) => {}
            Ok(Action::Reset) => {
                println!("ota_staged_reboot");
                // axstd's sockets are synchronous RAII handles; close the
                // listener before firmware takes over the machine reset.
                drop(stream);
                drop(listener);
                ax_std::os::arceos::api::modules::ax_hal::power::system_reset();
            }
            Ok(Action::Boot(execution)) => {
                println!(
                    "elf_loaded: load={:#x} end={:#x} entry={:#x} pages={}",
                    execution.elf.load_addr,
                    execution.elf.load_end,
                    execution.elf.entry_point,
                    execution.elf.page_count
                );
                println!("ready_to_handoff");
                // No background task owns the axstd sockets, so dropping the
                // listener is the complete transport shutdown for this
                // single-task frontend. The target receives the EFI context
                // only after the control connection is closed.
                drop(stream);
                drop(listener);
                if let Err(error) = execution.handoff() {
                    println!("jump_error: {error:?}");
                    ota.last_failure = Some("handoff_failed");
                    let _ = ota.rollback(Outcome::LoadFailed);
                }
                return;
            }
            Err(error) => {
                println!("loader_http_error: {error}");
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Interface {
    mac: MacAddress,
    current_mac: MacAddress,
}

impl BootTransport for Interface {
    fn mac_address(&self) -> MacAddress {
        self.mac
    }

    fn current_mac_address(&self) -> MacAddress {
        self.current_mac
    }
}

impl Interface {
    fn announce(&self, epoch: &str, serial_ready: bool) {
        announce(self.mac_address(), epoch, serial_ready);
    }
}

fn select_interface() -> Result<Interface, &'static str> {
    ax_net::interfaces()
        .into_iter()
        .filter_map(|info| info.mac.map(|mac| (info, mac)))
        .find_map(|(_, mac)| {
            let mut octets = [0; 6];
            octets.copy_from_slice(mac.as_bytes());
            let current = MacAddress::new(octets);
            (!current.is_zero()).then_some(Interface {
                mac: current,
                current_mac: current,
            })
        })
        .ok_or("no usable Ethernet interface")
}

fn announce(mac: MacAddress, epoch: &str, serial_ready: bool) {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return;
    };
    let Ok(payload) = serde_json::to_vec(&LoaderAnnouncement {
        protocol_version: DEVICE_PROTOCOL_VERSION,
        mac_address: mac,
        current_mac_address: mac,
        arch: BootArch::Loongarch64,
        loader_version: env!("CARGO_PKG_VERSION").into(),
        boot_epoch: epoch.into(),
        http_port: HTTP_PORT,
        serial_id: Some(epoch.into()),
        serial_ready,
    }) else {
        return;
    };
    if payload.len() > MAX_DISCOVERY_DATAGRAM_BYTES {
        return;
    }
    let _ = socket.send_to(
        &payload,
        SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT),
    );
}

fn boot_epoch(mac: MacAddress, fallback_identity: [u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(mac.octets());
    if let Some(seed) = someboot::boot_entropy() {
        hash.update(seed);
    } else {
        // Firmware without a trusted entropy source cannot provide a
        // cryptographic identity. The durable OTA generation is committed
        // before this value is published, so a reboot cannot replay it.
        hash.update(fallback_identity);
    }
    hex(&hash.finalize()[..16])
}

fn format_mac(mac: MacAddress) -> String {
    mac.octets()
        .iter()
        .enumerate()
        .fold(String::new(), |mut output, (index, byte)| {
            if index != 0 {
                output.push(':');
            }
            use core::fmt::Write;
            let _ = write!(output, "{byte:02x}");
            output
        })
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    use core::fmt::Write;
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

enum Action {
    None,
    Reset,
    Boot(BootExecution),
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn handle_connection(
    stream: &mut TcpStream,
    ota: &mut LaOta,
    server: &mut BootServer,
) -> Result<Action, String> {
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(error) => {
            let response = Response::json(error_status(&error), error_json(&error), Action::None);
            stream
                .write_all(response.bytes())
                .map_err(|_| "response write failed".to_string())?;
            return Ok(Action::None);
        }
    };
    let response = match route(request, ota, server) {
        Ok(response) => response,
        Err(error) => Response::json(error_status(&error), error_json(&error), Action::None),
    };
    stream
        .write_all(response.bytes())
        .map_err(|_| "response write failed".to_string())?;
    Ok(response.action)
}

fn error_status(error: &str) -> &'static str {
    match error {
        "unknown_boot_job" => "404 Not Found",
        "body too large" => "413 Payload Too Large",
        "request body too large" => "400 Bad Request",
        "request header too large" => "431 Request Header Fields Too Large",
        _ => "400 Bad Request",
    }
}

fn request_body_limit(method: &str, path: &str) -> usize {
    match (method, path) {
        ("POST", "/api/v1/serial/continue") | ("POST", "/api/v1/ota/confirm") => {
            MAX_SERIAL_BINDING_BYTES
        }
        ("POST", "/api/v1/boot/jobs") => MAX_HEADER,
        ("PUT", "/api/v1/ota/image") => MAX_IMAGE_BYTES,
        _ => MAX_IMAGE_BYTES.max(MAX_HTTP_BOOT_INITRAMFS_BYTES),
    }
}

fn error_json(error: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "error": error })).unwrap_or_else(|_| b"{}".to_vec())
}

fn parse_update_id(value: &str) -> Option<[u8; 36]> {
    let bytes: [u8; 36] = value.as_bytes().try_into().ok()?;
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
        .then_some(bytes)
}

fn valid_image_version(value: &str) -> bool {
    value.len() <= 96 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    let header_end = loop {
        if bytes.len() >= MAX_HEADER {
            return Err("request header too large".into());
        }
        let count = stream
            .read(&mut chunk)
            .map_err(|_| "request read failed".to_string())?;
        if count == 0 {
            return Err("request ended".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let header_end = index + 4;
            if header_end > MAX_HEADER {
                return Err("request header too large".into());
            }
            break header_end;
        }
    };
    let header = core::str::from_utf8(&bytes[..header_end])
        .map_err(|_| "request is not UTF-8".to_string())?;
    let mut lines = header.split("\r\n");
    let first = lines
        .next()
        .ok_or_else(|| "missing request line".to_string())?;
    let mut words = first.split_ascii_whitespace();
    let method = words.next().unwrap_or_default();
    let path = words.next().unwrap_or_default();
    let version = words.next().unwrap_or_default();
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") || words.next().is_some() {
        return Err("invalid HTTP request".to_string());
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines.take_while(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| "invalid HTTP header".to_string())?;
        headers.push((key.trim().into(), value.trim().into()));
    }
    let mut length = None;
    for (key, value) in &headers {
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("duplicate content length".to_string());
            }
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "invalid content length".to_string())?,
            );
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err("transfer encoding is unsupported".to_string());
        }
    }
    let length = length.unwrap_or(0);
    let body_limit = request_body_limit(method, path);
    if length > body_limit {
        let error = match (method, path) {
            ("POST", "/api/v1/serial/continue")
            | ("POST", "/api/v1/ota/confirm")
            | ("POST", "/api/v1/boot/jobs") => "request body too large",
            _ => "body too large",
        };
        return Err(error.to_string());
    }
    let mut body = bytes[header_end..].to_vec();
    if body.len() > length {
        return Err("body exceeds content length".to_string());
    }
    body.try_reserve(length.saturating_sub(body.len()))
        .map_err(|_| "body allocation failed".to_string())?;
    while body.len() < length {
        let count = stream
            .read(&mut chunk)
            .map_err(|_| "request read failed".to_string())?;
        if count == 0 {
            return Err("short body".to_string());
        }
        let remaining = length - body.len();
        if count > remaining {
            return Err("body exceeds content length".to_string());
        }
        body.extend_from_slice(&chunk[..count]);
    }
    Ok(HttpRequest {
        method: method.into(),
        path: path.into(),
        headers,
        body,
    })
}

struct Response {
    bytes: Vec<u8>,
    action: Action,
}

impl Response {
    fn json(status: &str, body: impl AsRef<[u8]>, action: Action) -> Self {
        let body = body.as_ref();
        let mut bytes = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
             {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        Self { bytes, action }
    }

    fn empty(status: &str) -> Self {
        Self::json(status, [], Action::None)
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn route(
    request: HttpRequest,
    ota: &mut LaOta,
    server: &mut BootServer,
) -> Result<Response, String> {
    let epoch = request.header("x-boot-epoch");
    if request.method == "GET" && request.path == "/api/v1/status" {
        let body = serde_json::to_vec(&LoaderDeviceStatus {
            protocol_version: DEVICE_PROTOCOL_VERSION,
            boot_epoch: server.epoch.clone(),
            mac_address: server.mac,
            current_mac_address: server.current_mac,
            arch: BootArch::Loongarch64,
            loader_version: env!("CARGO_PKG_VERSION").into(),
            hardware: LoaderHardwareInfo::default(),
            boot: server.status(),
            ota: Some(ota.protocol_state()),
            serial: Some(server.serial.clone()),
        })
        .map_err(|error| error.to_string())?;
        return Ok(Response::json("200 OK", body, Action::None));
    }
    if epoch != Some(server.epoch.as_str()) {
        return Ok(Response::json(
            "409 Conflict",
            br#"{"error":"stale_boot_epoch"}"#,
            Action::None,
        ));
    }
    if request.method == "POST" && request.path == "/api/v1/serial/continue" {
        let binding: httpboot_protocol::SerialBinding = serde_json::from_slice(&request.body)
            .map_err(|_| "invalid serial binding".to_string())?;
        let result = server.serial.grant(binding);
        return Ok(Response::json(
            if result.is_ok() {
                "200 OK"
            } else {
                "409 Conflict"
            },
            serde_json::to_vec(&server.serial).map_err(|error| error.to_string())?,
            Action::None,
        ));
    }
    if request.method == "DELETE"
        && let Some(binding_id) = request.path.strip_prefix("/api/v1/serial/bindings/")
    {
        let result = server.serial.revoke(binding_id);
        return Ok(Response::empty(if result.is_ok() {
            "200 OK"
        } else {
            "409 Conflict"
        }));
    }
    if request.method == "GET" && request.path == "/api/v1/ota/status" {
        return Ok(Response::json(
            "200 OK",
            serde_json::to_vec(&ota.status_json()).map_err(|error| error.to_string())?,
            Action::None,
        ));
    }
    if request.method == "POST" && request.path == "/api/v1/ota/confirm" {
        let value: serde_json::Value = serde_json::from_slice(&request.body)
            .map_err(|_| "invalid OTA confirmation".to_string())?;
        let update_id = value
            .get("update_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing OTA update id".to_string())?;
        if parse_update_id(update_id).is_none() {
            return Err("invalid OTA update id".to_string());
        }
        let source = match request.header("x-update-source") {
            Some("server") => OtaSource::Server,
            Some(_) => return Err("invalid OTA update source".to_string()),
            _ => OtaSource::Direct,
        };
        if ota.confirm(update_id, source).is_err() {
            return Ok(Response::json("409 Conflict", b"{}", Action::None));
        }
        return Ok(Response::json(
            "200 OK",
            serde_json::to_vec(&ota.status_json()).map_err(|error| error.to_string())?,
            Action::None,
        ));
    }
    if request.method == "PUT" && request.path == "/api/v1/ota/image" {
        if ota.state.pending.is_some() {
            return Ok(Response::json(
                "409 Conflict",
                br#"{"error":"ota_trial_requires_confirmation"}"#,
                Action::None,
            ));
        }
        if server.busy() {
            return Ok(Response::json(
                "409 Conflict",
                br#"{"error":"boot_job_busy"}"#,
                Action::None,
            ));
        }
        let Some(expected) = request
            .header("x-image-sha256")
            .and_then(integrity::decode_sha256)
        else {
            return Ok(Response::json("400 Bad Request", b"{}", Action::None));
        };
        if request.body.is_empty() || request.body.len() > MAX_IMAGE_BYTES {
            return Ok(Response::json("413 Payload Too Large", b"{}", Action::None));
        }
        let source = match request.header("x-update-source") {
            Some("server") => OtaSource::Server,
            Some(_) => return Err("invalid OTA update source".to_string()),
            _ => OtaSource::Direct,
        };
        let version = request.header("x-image-version");
        if version.is_some_and(|value| !valid_image_version(value)) {
            return Err("invalid OTA image version".to_string());
        }
        let update_id = match source {
            OtaSource::Server => request
                .header("x-update-id")
                .and_then(parse_update_id)
                .ok_or_else(|| "invalid OTA update id".to_string())?,
            OtaSource::Direct => ota.next_direct_id(&expected),
        };
        if ota
            .stage_image(&request.body, expected, update_id, version, source)
            .is_err()
        {
            return Ok(Response::json(
                "422 Unprocessable Content",
                br#"{"error":"image_digest_or_efi_validation_failed"}"#,
                Action::None,
            ));
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "update_id": core::str::from_utf8(&update_id).unwrap_or("")
        }))
        .map_err(|error| error.to_string())?;
        return Ok(Response::json("202 Accepted", body, Action::Reset));
    }
    if ota.trial() {
        return Ok(Response::json(
            "409 Conflict",
            br#"{"error":"ota_trial_requires_confirmation"}"#,
            Action::None,
        ));
    }
    if request.method == "POST" && request.path == "/api/v1/boot/jobs" {
        let manifest: DeviceBootJob = serde_json::from_slice(&request.body)
            .map_err(|_| "invalid boot manifest".to_string())?;
        let status = match server.create(manifest) {
            Ok(true) => "201 Created",
            Ok(false) => "200 OK",
            Err(_) => "409 Conflict",
        };
        return Ok(Response::json(
            status,
            serde_json::to_vec(&server.status()).map_err(|error| error.to_string())?,
            Action::None,
        ));
    }
    if let Some(tail) = request.path.strip_prefix("/api/v1/boot/jobs/") {
        let (id, operation) = tail.split_once('/').unwrap_or((tail, ""));
        if request.method == "GET" && operation.is_empty() {
            return Ok(Response::json(
                if server.descriptor(id, FileKind::Kernel).is_ok() {
                    "200 OK"
                } else {
                    "404 Not Found"
                },
                serde_json::to_vec(&server.status()).map_err(|error| error.to_string())?,
                Action::None,
            ));
        }
        if request.method == "DELETE" && operation.is_empty() {
            let status = if server.cancel(id).is_ok() {
                "204 No Content"
            } else {
                "404 Not Found"
            };
            return Ok(Response::empty(status));
        }
        if request.method == "PUT" && matches!(operation, "kernel" | "initramfs") {
            let kind = if operation == "kernel" {
                FileKind::Kernel
            } else {
                FileKind::Initramfs
            };
            let descriptor = server
                .descriptor(id, kind)
                .map_err(|error| error.to_string())?;
            if descriptor.size as usize != request.body.len()
                || request
                    .header("x-image-sha256")
                    .and_then(integrity::decode_sha256)
                    != integrity::decode_sha256(&descriptor.sha256)
            {
                let _ = server.cancel(id);
                return Ok(Response::json("400 Bad Request", b"{}", Action::None));
            }
            let result = server.upload(id, kind, request.body);
            return Ok(Response::json(
                if result.is_ok() {
                    "200 OK"
                } else {
                    "422 Unprocessable Content"
                },
                serde_json::to_vec(&server.status()).map_err(|error| error.to_string())?,
                Action::None,
            ));
        }
        if request.method == "POST" && operation == "start" {
            return match server.prepare(id, request.header("x-serial-binding")) {
                Ok(execution) => Ok(Response::json(
                    "202 Accepted",
                    br#"{"phase":"ready_to_handoff"}"#,
                    Action::Boot(execution),
                )),
                Err(error) => Ok(Response::json(
                    "409 Conflict",
                    serde_json::to_vec(&serde_json::json!({"error": error}))
                        .map_err(|error| error.to_string())?,
                    Action::None,
                )),
            };
        }
    }
    Ok(Response::json("404 Not Found", b"{}", Action::None))
}

struct BootServer {
    epoch: String,
    mac: MacAddress,
    current_mac: MacAddress,
    jobs: BootJobStore,
    serial: httpboot_protocol::LoaderSerialStatus,
}

impl BootServer {
    fn new(epoch: String, mac: MacAddress, current_mac: MacAddress) -> Self {
        let serial_id = epoch.clone();
        let parameters = active_serial_parameters();
        Self {
            epoch,
            mac,
            current_mac,
            jobs: BootJobStore::default(),
            serial: httpboot_protocol::LoaderSerialStatus {
                serial_id,
                ready: parameters.is_some(),
                parameters,
                binding: None,
                error: parameters
                    .is_none()
                    .then(|| "axstd runtime console has no active UART binding".into()),
            },
        }
    }

    fn serial_ready(&self) -> bool {
        self.serial.ready
    }

    fn serial_beacon(&self) {
        if self.serial.ready && self.serial.binding.is_none() {
            println!("AXLOADER-SERIAL/1 {}", self.serial.serial_id);
        }
    }

    fn busy(&self) -> bool {
        self.jobs.busy()
    }

    fn status(&self) -> Option<httpboot_protocol::DeviceBootStatus> {
        self.jobs.status()
    }

    fn create(&mut self, manifest: DeviceBootJob) -> Result<bool, &'static str> {
        self.jobs.create(
            manifest,
            LOONGARCH64_EFI,
            MAX_IMAGE_BYTES,
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
        )
    }

    fn descriptor(
        &self,
        id: &str,
        kind: FileKind,
    ) -> Result<&httpboot_protocol::DeviceBootImage, &'static str> {
        self.jobs.descriptor(id, kind)
    }

    fn upload(&mut self, id: &str, kind: FileKind, data: Vec<u8>) -> Result<(), &'static str> {
        self.jobs.upload(id, kind, data)
    }

    fn cancel(&mut self, id: &str) -> Result<(), &'static str> {
        self.jobs.cancel(id)
    }

    fn prepare(
        &mut self,
        id: &str,
        binding_id: Option<&str>,
    ) -> Result<BootExecution, &'static str> {
        if !binding_id.is_some_and(|binding| self.serial.permits_start(binding)) {
            return Err("serial_binding_required");
        }
        let job = self.jobs.job().ok_or("unknown_boot_job")?;
        if job.manifest.boot_id != id {
            return Err("unknown_boot_job");
        }
        let kernel = job.kernel.as_ref().ok_or("kernel_not_uploaded")?;
        if job.manifest.initramfs.is_some() && job.initramfs.is_none() {
            return Err("initramfs_not_uploaded");
        }
        if Sha256::digest(kernel).as_slice()
            != integrity::decode_sha256(&job.manifest.kernel.sha256)
                .as_ref()
                .ok_or("kernel_digest_mismatch")?
        {
            return Err("kernel_digest_mismatch");
        }
        let elf = LoadedElf::load(kernel, LOONGARCH64_EFI).map_err(|_| "kernel_load_failed")?;
        let payload = PublishedPayload::new(
            job.initramfs.as_deref(),
            job.manifest.cmdline.as_deref(),
            Some((elf.load_addr as usize, elf.load_end as usize)),
        )
        .map_err(|_| "host_payload_failed")?;
        Ok(BootExecution { elf, payload })
    }
}

fn active_serial_parameters() -> Option<SerialParameters> {
    let runtime = std::os::arceos::modules::ax_runtime::serial::runtimes()
        .iter()
        .find(|runtime| std::os::arceos::modules::ax_runtime::console::is_active(runtime))?;
    let baud_rate = u64::from(runtime.info().initial_baudrate);
    // The runtime keeps the firmware's adopted line settings; only the
    // baud-rate field is exposed by SerialDeviceInfo. The remaining fields
    // use the protocol's documented 8N1/no-flow-control default.
    (baud_rate != 0).then(|| SerialParameters {
        baud_rate,
        ..SerialParameters::default()
    })
}

#[derive(Debug)]
struct LoadedElf {
    entry_point: u64,
    load_addr: u64,
    load_end: u64,
    page_count: usize,
}

#[derive(Debug)]
enum ElfError {
    Invalid,
    UnsupportedMachine,
    InvalidEntry,
    AddressConflict,
    Allocation,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Header {
    ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64ProgramHeader {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64SectionHeader {
    sh_name: u32,
    sh_type: u32,
    sh_flags: u64,
    sh_addr: u64,
    sh_offset: u64,
    sh_size: u64,
    sh_link: u32,
    sh_info: u32,
    sh_addralign: u64,
    sh_entsize: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Symbol {
    st_name: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
    st_value: u64,
    st_size: u64,
}

#[derive(Clone, Copy)]
struct LoadSegment {
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
}

impl LoadedElf {
    fn load(image: &[u8], spec: axloader::target::TargetSpec) -> Result<Self, ElfError> {
        let header = read_struct::<Elf64Header>(image, 0).ok_or(ElfError::Invalid)?;
        if &header.ident[..4] != b"\x7fELF"
            || header.ident[4] != 2
            || header.ident[5] != 1
            || header.ident[6] != 1
            || header.e_version != 1
        {
            return Err(ElfError::Invalid);
        }
        if !spec.matches_machine(header.e_machine) {
            return Err(ElfError::UnsupportedMachine);
        }
        let segments = load_segments(image, &header)?;
        let link_addr = segments
            .iter()
            .map(|segment| segment.paddr)
            .min()
            .ok_or(ElfError::Invalid)?;
        let load_end = segments
            .iter()
            .map(|segment| segment.paddr.checked_add(segment.memsz))
            .collect::<Option<Vec<_>>>()
            .and_then(|values| values.into_iter().max())
            .ok_or(ElfError::Invalid)?;
        let link_addr = align_down(link_addr, 4096);
        let load_end = align_up(load_end, 4096).ok_or(ElfError::Invalid)?;
        let page_count =
            usize::try_from((load_end - link_addr) / 4096).map_err(|_| ElfError::Invalid)?;
        let entry = find_symbol(image, &header, spec.entry_symbol)
            .and_then(|value| virtual_to_executable_physical(value, &segments))
            .ok_or(ElfError::InvalidEntry)?;
        let target_phys = LoaderImageMemory
            .allocate_image(page_count)
            .map_err(|_| ElfError::AddressConflict)?;
        let target_end = match target_phys.checked_add(load_end as usize - link_addr as usize) {
            Some(end) => end,
            None => {
                someboot::loader_release_image(target_phys, page_count);
                return Err(ElfError::AddressConflict);
            }
        };
        let (loader_start, loader_end) = match someboot::loader_image_range() {
            Some(range) => range,
            None => {
                someboot::loader_release_image(target_phys, page_count);
                return Err(ElfError::AddressConflict);
            }
        };
        if target_phys < loader_end && loader_start < target_end {
            someboot::loader_release_image(target_phys, page_count);
            return Err(ElfError::AddressConflict);
        }
        if spec
            .link_address
            .is_some_and(|expected| expected != link_addr)
            || spec
                .load_address
                .is_some_and(|expected| expected != target_phys as u64)
        {
            someboot::loader_release_image(target_phys, page_count);
            return Err(ElfError::AddressConflict);
        }
        let target = match NonNull::new(someboot::mem::phys_to_virt(target_phys)) {
            Some(target) => target,
            None => {
                someboot::loader_release_image(target_phys, page_count);
                return Err(ElfError::Allocation);
            }
        };
        let allocation_bytes = match page_count.checked_mul(4096) {
            Some(bytes) => bytes,
            None => {
                someboot::loader_release_image(target_phys, page_count);
                return Err(ElfError::Invalid);
            }
        };
        // The fixed image pool may be reused after a failed handoff. Clear the
        // complete allocation so alignment gaps between PT_LOAD segments never
        // expose bytes left by an earlier image.
        // SAFETY: `target` is the non-null virtual alias returned for the
        // contiguous `allocation_bytes` image allocation, and the validated
        // page count keeps the write within that allocation.
        unsafe { core::ptr::write_bytes(target.as_ptr(), 0, allocation_bytes) };
        if copy_segments(image, &segments, link_addr, target).is_err() {
            someboot::loader_release_image(target_phys, page_count);
            return Err(ElfError::Invalid);
        }
        someboot::loader_flush_icache();
        Ok(Self {
            entry_point: someboot::mem::phys_to_virt(target_phys + (entry - link_addr) as usize)
                as u64,
            load_addr: target_phys as u64,
            load_end: target_end as u64,
            page_count,
        })
    }
}

impl Drop for LoadedElf {
    fn drop(&mut self) {
        let _ = someboot::loader_release_image(self.load_addr as usize, self.page_count);
    }
}

#[derive(Debug)]
struct LoadedPe {
    entry_point: u64,
    load_addr: usize,
    load_end: usize,
    page_count: usize,
}

impl LoadedPe {
    /// Returns the PE32+ ImageBase field offset after checking its fixed header.
    fn image_base_offset(image: &[u8]) -> Result<usize, &'static str> {
        if image.len() < 0x40 || &image[..2] != b"MZ" {
            return Err("invalid EFI image signature");
        }
        let pe_offset = read_u32(image, 0x3c)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or("invalid EFI PE offset")?;
        let optional = pe_offset.checked_add(24).ok_or("EFI PE offset overflows")?;
        let optional_size = usize::from(
            read_u16(
                image,
                pe_offset.checked_add(20).ok_or("EFI PE offset overflows")?,
            )
            .ok_or("missing EFI PE optional header size")?,
        );
        if pe_offset
            .checked_add(24)
            .and_then(|offset| offset.checked_add(optional_size))
            .is_none_or(|end| end > image.len())
            || read_u32(image, pe_offset) != Some(u32::from_le_bytes(*b"PE\0\0"))
            || read_u16(image, optional) != Some(PE32_PLUS_MAGIC)
            || optional_size < PE32_PLUS_OPTIONAL_HEADER_SIZE
        {
            return Err("unsupported EFI PE optional header");
        }
        optional
            .checked_add(24)
            .ok_or("EFI PE image base offset overflows")
    }

    /// Canonicalizes a firmware-loaded image before it becomes an OTA slot.
    ///
    /// Some UEFI PE loaders rewrite the in-memory ImageBase field with the
    /// address selected for the image. The on-disk someboot image deliberately
    /// uses ImageBase = 0, so clear only this firmware-owned header field while
    /// preserving the already loaded sections and relocation table.
    fn canonicalize_loaded_image(image: &mut [u8]) -> Result<(), &'static str> {
        let image_base = Self::image_base_offset(image)?;
        image[image_base..image_base + 8].fill(0);
        Ok(())
    }

    fn validate(image: &[u8]) -> Result<(), &'static str> {
        let (headers, sections, image_size, entry) = pe_layout(image)?;
        if headers > image_size || entry < headers || entry >= image_size {
            return Err("invalid EFI image layout");
        }
        let mut virtual_ranges = Vec::with_capacity(sections.len());
        let mut entry_is_executable = false;
        for section in sections {
            let virtual_end = section
                .virtual_address
                .checked_add(section.virtual_size.max(section.raw_size))
                .ok_or("EFI section range overflows")?;
            if virtual_end > image_size {
                return Err("EFI section is outside the image");
            }
            let raw_end = section
                .raw_offset
                .checked_add(section.raw_size)
                .ok_or("EFI section file range overflows")?;
            if raw_end > image.len() {
                return Err("EFI section is outside the file");
            }
            if section.virtual_address < headers {
                return Err("EFI section overlaps PE headers");
            }
            if section.characteristics & PE_SECTION_EXECUTE != 0
                && entry >= section.virtual_address
                && entry < virtual_end
            {
                entry_is_executable = true;
            }
            virtual_ranges.push((section.virtual_address, virtual_end));
        }
        if !entry_is_executable {
            return Err("EFI entry is outside executable sections");
        }
        virtual_ranges.sort_unstable_by_key(|(start, _)| *start);
        if virtual_ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err("EFI sections overlap");
        }
        Ok(())
    }

    fn load(image: &[u8]) -> Result<Self, &'static str> {
        Self::validate(image)?;
        let (headers, sections, image_size, entry) = pe_layout(image)?;
        let page_count = image_size.div_ceil(4096);
        let load_addr = someboot::loader_allocate_slot(page_count)
            .ok_or("EFI slot image pool is unavailable")?;
        let target = someboot::mem::phys_to_virt(load_addr);
        // SAFETY: the slot pool is reserved by someboot and `image_size` stays
        // inside the fixed allocation returned above.
        unsafe {
            core::ptr::write_bytes(target, 0, page_count * 4096);
            core::ptr::copy_nonoverlapping(image.as_ptr(), target, headers);
        }
        for section in sections {
            // SAFETY: `pe_layout` checked each section's virtual range against
            // `image_size`, and `target` points to the allocation of
            // `page_count * 4096` bytes covering that complete image.
            let destination = unsafe { target.add(section.virtual_address) };
            if section.raw_size != 0 {
                // SAFETY: pe_layout checked the raw range and the destination
                // section range against the allocated image.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        image.as_ptr().add(section.raw_offset),
                        destination,
                        section.raw_size,
                    );
                }
            }
        }
        someboot::loader_flush_icache();
        Ok(Self {
            entry_point: someboot::mem::phys_to_virt(load_addr + entry) as u64,
            load_addr,
            load_end: load_addr + page_count * 4096,
            page_count,
        })
    }
}

impl Drop for LoadedPe {
    fn drop(&mut self) {
        let _ = someboot::loader_release_slot(self.load_addr, self.page_count);
    }
}

#[derive(Clone, Copy)]
struct PeSection {
    virtual_address: usize,
    virtual_size: usize,
    raw_offset: usize,
    raw_size: usize,
    characteristics: u32,
}

fn pe_layout(image: &[u8]) -> Result<(usize, Vec<PeSection>, usize, usize), &'static str> {
    if image.len() < 0x40 || &image[..2] != b"MZ" {
        return Err("invalid EFI image signature");
    }
    let pe_offset = read_u32(image, 0x3c)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or("invalid EFI PE offset")?;
    if pe_offset
        .checked_add(24)
        .is_none_or(|end| end > image.len())
        || &image[pe_offset..pe_offset + 4] != b"PE\0\0"
    {
        return Err("invalid EFI PE signature");
    }
    let machine = read_u16(image, pe_offset + 4).ok_or("missing EFI PE machine")?;
    if machine != PE_MACHINE_LOONGARCH64 {
        return Err("unsupported EFI PE machine");
    }
    let section_count =
        usize::from(read_u16(image, pe_offset + 6).ok_or("missing EFI PE section count")?);
    let optional_size =
        usize::from(read_u16(image, pe_offset + 20).ok_or("missing EFI optional header size")?);
    let optional = pe_offset.checked_add(24).ok_or("EFI PE offset overflows")?;
    if optional
        .checked_add(optional_size)
        .is_none_or(|end| end > image.len())
        || read_u16(image, optional) != Some(PE32_PLUS_MAGIC)
    {
        return Err("unsupported EFI PE optional header");
    }
    if optional_size < PE32_PLUS_OPTIONAL_HEADER_SIZE {
        return Err("truncated EFI PE optional header");
    }
    if read_u64(image, optional + 24) != Some(0) {
        return Err("unsupported EFI image base");
    }
    let directory_count =
        read_u32(image, optional + 108).ok_or("missing EFI PE data-directory count")?;
    if directory_count > PE_BASE_RELOCATION_DIRECTORY as u32 {
        let relocation_directory = optional
            .checked_add(PE_DATA_DIRECTORY_OFFSET)
            .and_then(|offset| offset.checked_add(PE_BASE_RELOCATION_DIRECTORY.checked_mul(8)?))
            .ok_or("EFI PE relocation directory overflows")?;
        if relocation_directory
            .checked_add(8)
            .is_none_or(|end| end > optional + optional_size)
        {
            return Err("truncated EFI PE relocation directory");
        }
        if read_u32(image, relocation_directory) != Some(0)
            || read_u32(image, relocation_directory + 4) != Some(0)
        {
            return Err("EFI PE base relocations are unsupported");
        }
    }
    let entry = usize::try_from(read_u32(image, optional + 16).ok_or("missing EFI PE entry")?)
        .map_err(|_| "EFI PE entry overflows")?;
    let image_size = usize::try_from(read_u32(image, optional + 56).ok_or("missing EFI PE size")?)
        .map_err(|_| "EFI PE size overflows")?;
    let headers = usize::try_from(read_u32(image, optional + 60).ok_or("missing EFI PE headers")?)
        .map_err(|_| "EFI PE headers overflow")?;
    if image_size == 0 || image_size > MAX_IMAGE_BYTES || headers > image.len() {
        return Err("invalid EFI PE size");
    }
    let section_table = optional
        .checked_add(optional_size)
        .ok_or("EFI section table overflows")?;
    let section_bytes = section_count
        .checked_mul(PE_SECTION_HEADER_SIZE)
        .ok_or("EFI section table size overflows")?;
    if section_table
        .checked_add(section_bytes)
        .is_none_or(|end| end > image.len())
    {
        return Err("EFI section table is truncated");
    }
    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let offset = section_table + index * PE_SECTION_HEADER_SIZE;
        let virtual_size =
            usize::try_from(read_u32(image, offset + 8).ok_or("missing EFI section size")?)
                .map_err(|_| "EFI section size overflows")?;
        let virtual_address =
            usize::try_from(read_u32(image, offset + 12).ok_or("missing EFI section address")?)
                .map_err(|_| "EFI section address overflows")?;
        let raw_size =
            usize::try_from(read_u32(image, offset + 16).ok_or("missing EFI section raw size")?)
                .map_err(|_| "EFI section raw size overflows")?;
        let raw_offset =
            usize::try_from(read_u32(image, offset + 20).ok_or("missing EFI section raw offset")?)
                .map_err(|_| "EFI section raw offset overflows")?;
        let characteristics =
            read_u32(image, offset + 36).ok_or("missing EFI section characteristics")?;
        sections.push(PeSection {
            virtual_address,
            virtual_size,
            raw_offset,
            raw_size,
            characteristics,
        });
    }
    Ok((headers, sections, image_size, entry))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    bytes
        .get(offset..offset.checked_add(2)?)
        .map(|value| u16::from_le_bytes(value.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset.checked_add(4)?)
        .map(|value| u32::from_le_bytes(value.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    bytes
        .get(offset..offset.checked_add(8)?)
        .map(|value| u64::from_le_bytes(value.try_into().unwrap()))
}

fn load_segments(image: &[u8], header: &Elf64Header) -> Result<Vec<LoadSegment>, ElfError> {
    if usize::from(header.e_phentsize) != mem::size_of::<Elf64ProgramHeader>() {
        return Err(ElfError::Invalid);
    }
    let offset = usize::try_from(header.e_phoff).map_err(|_| ElfError::Invalid)?;
    let size = usize::from(header.e_phnum)
        .checked_mul(usize::from(header.e_phentsize))
        .ok_or(ElfError::Invalid)?;
    if offset.checked_add(size).ok_or(ElfError::Invalid)? > image.len() {
        return Err(ElfError::Invalid);
    }
    let mut segments = Vec::new();
    for index in 0..usize::from(header.e_phnum) {
        let ph = read_struct::<Elf64ProgramHeader>(
            image,
            offset + index * usize::from(header.e_phentsize),
        )
        .ok_or(ElfError::Invalid)?;
        if ph.p_type != 1 || ph.p_memsz == 0 {
            continue;
        }
        if ph.p_filesz > ph.p_memsz || ph.p_paddr % 4096 != 0 {
            return Err(ElfError::Invalid);
        }
        let file_start = usize::try_from(ph.p_offset).map_err(|_| ElfError::Invalid)?;
        let file_size = usize::try_from(ph.p_filesz).map_err(|_| ElfError::Invalid)?;
        if file_start.checked_add(file_size).ok_or(ElfError::Invalid)? > image.len() {
            return Err(ElfError::Invalid);
        }
        segments.push(LoadSegment {
            offset: ph.p_offset,
            vaddr: ph.p_vaddr,
            paddr: ph.p_paddr,
            filesz: ph.p_filesz,
            memsz: ph.p_memsz,
            flags: ph.p_flags,
        });
    }
    segments.sort_by_key(|segment| segment.paddr);
    if segments
        .windows(2)
        .any(|pair| axloader::target::ranges_overlap(pair[0].paddr, pair[0].memsz, pair[1].paddr))
    {
        return Err(ElfError::Invalid);
    }
    Ok(segments)
}

fn copy_segments(
    image: &[u8],
    segments: &[LoadSegment],
    load_addr: u64,
    target: NonNull<u8>,
) -> Result<(), ElfError> {
    for segment in segments {
        let offset = usize::try_from(segment.paddr - load_addr).map_err(|_| ElfError::Invalid)?;
        // SAFETY: `load_segments` rejects overlapping/out-of-range PT_LOAD
        // ranges, and the image allocation covers the complete aligned range
        // from `load_addr` through every segment end.
        let destination = unsafe { target.as_ptr().add(offset) };
        let source = usize::try_from(segment.offset).map_err(|_| ElfError::Invalid)?;
        let filesz = usize::try_from(segment.filesz).map_err(|_| ElfError::Invalid)?;
        let memsz = usize::try_from(segment.memsz).map_err(|_| ElfError::Invalid)?;
        if filesz != 0 {
            // SAFETY: load_segments checked the source range and the allocated target covers all
            // PT_LOAD ranges.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image[source..source + filesz].as_ptr(),
                    destination,
                    filesz,
                );
            }
        }
        if memsz > filesz {
            // SAFETY: `filesz <= memsz` and the validated PT_LOAD range keeps
            // both the start and the zeroed tail within the allocation.
            unsafe { core::ptr::write_bytes(destination.add(filesz), 0, memsz - filesz) };
        }
    }
    Ok(())
}

fn virtual_to_executable_physical(value: u64, segments: &[LoadSegment]) -> Option<u64> {
    segments.iter().find_map(|segment| {
        let end = segment.vaddr.checked_add(segment.memsz)?;
        (segment.flags & ELF_PF_X != 0 && value >= segment.vaddr && value < end)
            .then(|| segment.paddr + (value - segment.vaddr))
    })
}

fn find_symbol(image: &[u8], header: &Elf64Header, name: &str) -> Option<u64> {
    let section_size = usize::from(header.e_shentsize);
    let section_offset = usize::try_from(header.e_shoff).ok()?;
    if section_size < mem::size_of::<Elf64SectionHeader>() {
        return None;
    }
    for index in 0..usize::from(header.e_shnum) {
        let section = read_struct::<Elf64SectionHeader>(
            image,
            section_offset.checked_add(index.checked_mul(section_size)?)?,
        )?;
        if section.sh_type != 2 && section.sh_type != 11 {
            continue;
        }
        let strtab = read_struct::<Elf64SectionHeader>(
            image,
            section_offset.checked_add(
                usize::try_from(section.sh_link)
                    .ok()?
                    .checked_mul(section_size)?,
            )?,
        )?;
        let symbols = section_bytes(image, section.sh_offset, section.sh_size)?;
        let strings = section_bytes(image, strtab.sh_offset, strtab.sh_size)?;
        let entry_size = usize::try_from(section.sh_entsize)
            .ok()?
            .max(mem::size_of::<Elf64Symbol>());
        for offset in (0..symbols.len()).step_by(entry_size) {
            let symbol = read_struct::<Elf64Symbol>(symbols, offset)?;
            let start = usize::try_from(symbol.st_name).ok()?;
            let tail = strings.get(start..)?;
            let end = tail.iter().position(|byte| *byte == 0)?;
            if core::str::from_utf8(&tail[..end]).ok()? == name {
                return Some(symbol.st_value);
            }
        }
    }
    None
}

fn section_bytes(image: &[u8], offset: u64, size: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let size = usize::try_from(size).ok()?;
    image.get(start..start.checked_add(size)?)
}

fn read_struct<T: Copy>(image: &[u8], offset: usize) -> Option<T> {
    let end = offset.checked_add(mem::size_of::<T>())?;
    let bytes = image.get(offset..end)?;
    // SAFETY: `bytes` has exactly `size_of::<T>()` bytes, its pointer is valid
    // for reads, and `read_unaligned` does not require alignment. Every caller
    // uses a `Copy` POD header whose bit patterns are valid values.
    Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast()) })
}

fn align_down(value: u64, align: u64) -> u64 {
    value / align * align
}

fn align_up(value: u64, align: u64) -> Option<u64> {
    value
        .checked_add(align - 1)
        .map(|value| align_down(value, align))
}

struct PublishedPayload;

struct LoaderImageMemory;

impl ImageMemory for LoaderImageMemory {
    type Error = &'static str;

    fn allocate_image(&self, pages: usize) -> Result<usize, Self::Error> {
        someboot::loader_allocate_image(pages).ok_or("EFI image pool is unavailable")
    }
}

impl PublishedPayload {
    fn new(
        initramfs: Option<&[u8]>,
        cmdline: Option<&str>,
        target_image: Option<(usize, usize)>,
    ) -> Result<Self, &'static str> {
        if let Some(cmdline) = cmdline
            && !httpboot_protocol::valid_host_cmdline(cmdline)
        {
            return Err("invalid command line");
        }
        someboot::loader_set_payload_with_image(cmdline, initramfs, target_image)?;
        Ok(Self)
    }
}

impl Drop for PublishedPayload {
    fn drop(&mut self) {
        let _ = someboot::loader_set_payload_with_image(None, None, None);
    }
}

struct BootExecution {
    elf: LoadedElf,
    payload: PublishedPayload,
}

struct LoaderEfiHandoff;

impl EfiHandoff for LoaderEfiHandoff {
    type Error = Status;

    fn handoff(&self, entry_point: u64) -> Result<(), Self::Error> {
        let (handle, system_table) = someboot::loader_efi_context().ok_or(Status::NOT_FOUND)?;
        let status = unsafe { call_efi_entry(entry_point, handle, system_table) }?;
        (status == Status::SUCCESS).then_some(()).ok_or(status)
    }
}

impl BootExecution {
    fn handoff(self) -> Result<(), Status> {
        let result = LoaderEfiHandoff.handoff(self.elf.entry_point);
        drop(self.payload);
        result
    }
}

/// Calls a validated LoongArch EFI image entry point.
///
/// # Safety
/// `entry_point` must be the `efi_pe_entry` address found by `LoadedElf` or
/// `LoadedPe` inside an executable, validated image range. The target image
/// allocation must remain live and unmoved for the duration of the call, and
/// `image_handle`/`system_table` must be the current firmware EFI context.
unsafe fn call_efi_entry(
    entry_point: u64,
    image_handle: uefi::Handle,
    system_table: *const c_void,
) -> Result<Status, Status> {
    let entry: extern "efiapi" fn(uefi::Handle, *const c_void) -> Status =
        // SAFETY: LoadedElf found the exported efi_pe_entry symbol inside an executable PT_LOAD
        // range, and this LoongArch someboot symbol has the EFI ABI above.
        unsafe { mem::transmute(entry_point as usize) };
    Ok(entry(image_handle, system_table))
}

struct LaOta {
    state: State,
    index: usize,
    slot: Slot,
    last_failure: Option<&'static str>,
}

impl LaOta {
    fn open(current_image: &[u8]) -> Self {
        fs::create_dir_all(OTA_ROOT)
            .unwrap_or_else(|_| panic!("ota_bootstrap: failed to access OTA directory"));
        Self::cleanup_temporary_slots();
        let (state, index) =
            match Self::load_state().unwrap_or_else(|error| panic!("ota_bootstrap: {error}")) {
                Some(state) => state,
                None => {
                    let path = Self::slot_path(Slot::A);
                    let digest = match Self::read_file_if_present(&path)
                        .unwrap_or_else(|error| panic!("ota_bootstrap: {error}"))
                    {
                        Some(bytes) => {
                            LoadedPe::validate(&bytes).unwrap_or_else(|error| {
                                panic!("ota_bootstrap: invalid stable A.EFI: {error}")
                            });
                            Sha256::digest(&bytes).into()
                        }
                        None => {
                            Self::persist_slot_image(Slot::A, current_image).unwrap_or_else(
                                |error| panic!("ota_bootstrap: failed to persist A.EFI: {error}"),
                            );
                            Sha256::digest(current_image).into()
                        }
                    };
                    let state = State::initial(digest).expect("nonzero initial OTA digest");
                    let index = Self::write_state(0, &state).unwrap_or_else(|error| {
                        panic!("ota_bootstrap: failed to persist state: {error}")
                    });
                    (state, index)
                }
            };
        Self {
            slot: state.active,
            state,
            index,
            last_failure: None,
        }
    }

    fn set_running_slot(&mut self, slot: u8) -> Result<(), &'static str> {
        self.slot = match slot {
            0 => Slot::A,
            1 => Slot::B,
            _ => return Err("invalid inherited OTA slot"),
        };
        if self.state.pending == Some(self.slot) && !self.state.attempted {
            return Err("inherited OTA slot was not marked attempted");
        }
        Ok(())
    }

    fn advance_boot(&mut self) -> Result<(), &'static str> {
        let next = self
            .state
            .advance_boot()
            .map_err(|_| "OTA boot generation overflow")?;
        self.commit(&next)
    }

    fn boot_identity(&self, current_digest: [u8; 32]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"axloader-loongarch-boot-v1");
        hash.update(self.state.generation.to_le_bytes());
        hash.update(current_digest);
        hash.finalize().into()
    }

    fn prepare_bootstrap(
        &mut self,
        current_digest: [u8; 32],
    ) -> Result<Option<Slot>, &'static str> {
        if self.state.attempted {
            let next = self
                .state
                .rollback(Outcome::RolledBack)
                .map_err(|_| "invalid OTA rollback state")?;
            self.commit(&next)?;
        }
        let mut candidate = self.state.pending.unwrap_or(self.state.active);
        if !self.slot_matches_state(candidate)? {
            if self.state.pending == Some(candidate) {
                let next = self
                    .state
                    .rollback(Outcome::LoadFailed)
                    .map_err(|_| "invalid OTA candidate state")?;
                self.commit(&next)?;
                candidate = self.state.active;
            } else {
                return Err("active OTA slot is unavailable");
            }
        }
        if self.state.pending == Some(candidate) {
            let next = self
                .state
                .mark_attempt()
                .map_err(|_| "invalid OTA trial state")?;
            self.commit(&next)?;
        }
        self.slot = candidate;
        // The LoongArch EFI entry is the loader itself, rather than a separate
        // launcher. Continue in place when firmware already loaded the selected
        // slot; only invoke the slot image when the firmware image is different.
        if current_digest.as_slice() == self.state.digest(candidate) {
            Ok(None)
        } else {
            Ok(Some(candidate))
        }
    }

    fn slot_matches_state(&self, slot: Slot) -> Result<bool, &'static str> {
        let path = Self::slot_path(slot);
        let Some(bytes) = Self::read_file_if_present(&path)? else {
            return Ok(false);
        };
        Ok(Sha256::digest(&bytes).as_slice() == self.state.digest(slot))
    }

    fn persist_slot_image(slot: Slot, image: &[u8]) -> Result<(), &'static str> {
        // Preserve the parser's concrete reason so a bad firmware image or an
        // incorrect physical alias is diagnosable before it is persisted.
        LoadedPe::validate(image)?;
        let path = Self::slot_path(slot);
        let temporary = Self::slot_temporary_path(slot);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|_| "initial slot create failed")?;
        file.write_all(image)
            .map_err(|_| "initial slot write failed")?;
        file.flush().map_err(|_| "initial slot flush failed")?;
        drop(file);
        // axstd's FAT rename does not replace an existing directory entry. Remove the
        // inactive slot only after the complete temporary image has been flushed; the
        // state record still points at the previous stable slot if the rename fails.
        let _ = fs::remove_file(&path);
        fs::rename(&temporary, &path).map_err(|_| "initial slot commit failed")?;
        let readback = fs::read(&path).map_err(|_| "initial slot readback failed")?;
        if readback != image || LoadedPe::validate(&readback).is_err() {
            return Err("initial slot readback mismatch");
        }
        Ok(())
    }

    fn launch_slot(&mut self, slot: Slot) -> Result<(), &'static str> {
        let path = Self::slot_path(slot);
        let image = fs::read(&path).map_err(|_| "OTA slot read failed")?;
        if Sha256::digest(&image).as_slice() != self.state.digest(slot) {
            return Err("OTA slot digest mismatch");
        }
        let loaded = LoadedPe::load(&image)?;
        let _payload = PublishedPayload::new(
            None,
            someboot::cmdline(),
            Some((loaded.load_addr, loaded.load_end)),
        )?;
        someboot::loader_set_slot(Some(slot as u8))?;
        let (handle, system_table) =
            someboot::loader_efi_context().ok_or("EFI context is unavailable")?;
        let status = unsafe { call_efi_entry(loaded.entry_point, handle, system_table) }
            .map_err(|_| "OTA slot EFI entry failed")?;
        if status == Status::SUCCESS {
            Err("OTA slot returned successfully")
        } else {
            Err("OTA slot EFI entry failed")
        }
    }

    fn load_state() -> Result<Option<(State, usize)>, &'static str> {
        let first_path = format!("{OTA_ROOT}/STATE0.BIN");
        let second_path = format!("{OTA_ROOT}/STATE1.BIN");
        let first = Self::read_file_if_present(&first_path)?;
        let second = Self::read_file_if_present(&second_path)?;
        if first.is_none() && second.is_none() {
            return Ok(None);
        }
        let first = first.unwrap_or_default();
        let second = second.unwrap_or_default();
        ota::newest(&first, &second)
            .map(Some)
            .map_err(|_| "both OTA state records are invalid")
    }

    fn read_file_if_present(path: &str) -> Result<Option<Vec<u8>>, &'static str> {
        let (directory, name) = path.rsplit_once('/').ok_or("OTA path has no directory")?;
        let entries = fs::read_dir(directory).map_err(|_| "OTA directory read failed")?;
        let mut present = false;
        for entry in entries {
            let entry = entry.map_err(|_| "OTA directory read failed")?;
            if entry.file_name().as_str().eq_ignore_ascii_case(name) {
                present = true;
                break;
            }
        }
        if !present {
            return Ok(None);
        }
        fs::read(path).map(Some).map_err(|_| "OTA file read failed")
    }

    fn write_state(index: usize, state: &State) -> Result<usize, &'static str> {
        let target = 1 - index;
        let path = format!("{OTA_ROOT}/STATE{target}.BIN");
        let encoded = state.encode();
        let mut file = File::create(&path).map_err(|_| "state create failed")?;
        file.write_all(&encoded).map_err(|_| "state write failed")?;
        file.flush().map_err(|_| "state flush failed")?;
        drop(file);
        let readback = fs::read(&path).map_err(|_| "state readback failed")?;
        let decoded = State::decode(&readback).map_err(|_| "state checksum failed")?;
        if decoded != *state || readback.as_slice() != encoded.as_slice() {
            return Err("state readback mismatch");
        }
        Ok(target)
    }

    fn commit(&mut self, state: &State) -> Result<(), &'static str> {
        <Self as OtaStorage>::commit_state(self, state)
    }

    fn commit_inner(&mut self, state: &State) -> Result<(), &'static str> {
        self.index = Self::write_state(self.index, state)?;
        self.state = state.clone();
        Ok(())
    }

    fn trial(&self) -> bool {
        self.state.pending == Some(self.slot) && self.state.attempted
    }

    fn rollback(&mut self, outcome: Outcome) -> Result<(), &'static str> {
        let next = self
            .state
            .rollback(outcome)
            .map_err(|_| "invalid OTA rollback")?;
        self.commit(&next)?;
        self.slot = self.state.active;
        Ok(())
    }

    fn next_direct_id(&self, digest: &[u8; 32]) -> [u8; 36] {
        let mut hash = Sha256::new();
        hash.update(self.state.generation.to_le_bytes());
        hash.update(self.state.digest(self.state.active));
        hash.update(digest);
        let value = hex(&hash.finalize()[..16]);
        format!(
            "{}-{}-{}-{}-{}",
            &value[..8],
            &value[8..12],
            &value[12..16],
            &value[16..20],
            &value[20..32]
        )
        .as_bytes()
        .try_into()
        .expect("UUID text has 36 bytes")
    }

    fn stage_image(
        &mut self,
        image: &[u8],
        digest: [u8; 32],
        update_id: [u8; 36],
        version: Option<&str>,
        source: OtaSource,
    ) -> Result<(), &'static str> {
        if Sha256::digest(image).as_slice() != digest || self.state.pending.is_some() {
            return Err("invalid OTA image");
        }
        // Validate the image before replacing the inactive slot. A failed
        // upload must not leave an unbootable candidate behind.
        LoadedPe::validate(image).map_err(|_| "invalid LoongArch EFI image")?;
        let target = self.state.active.other();
        let path = Self::slot_path(target);
        let temporary = Self::slot_temporary_path(target);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|_| "OTA image create failed")?;
        file.write_all(image)
            .map_err(|_| "OTA image write failed")?;
        file.flush().map_err(|_| "OTA image flush failed")?;
        drop(file);
        // Keep the replacement name within FAT's short-name format. The state is
        // committed only after readback validation, so an interrupted replacement
        // remains a rejected inactive candidate on the next boot.
        let _ = fs::remove_file(&path);
        fs::rename(&temporary, &path).map_err(|_| "OTA image commit failed")?;
        let readback = fs::read(&path).map_err(|_| "OTA image readback failed")?;
        if Sha256::digest(&readback).as_slice() != digest || LoadedPe::validate(&readback).is_err()
        {
            return Err("OTA image readback mismatch");
        }
        let next = self
            .state
            .stage_named(digest, update_id, source, version)
            .map_err(|_| "invalid OTA state")?;
        self.commit(&next)
    }

    fn confirm(&mut self, update_id: &str, source: OtaSource) -> Result<(), &'static str> {
        let id = parse_update_id(update_id).ok_or("invalid update id")?;
        let next = self
            .state
            .confirm(self.slot, &id, source)
            .map_err(|_| "stale OTA confirmation")?;
        self.commit(&next)
    }

    fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "running_slot": slot_text(self.slot),
            "active_slot": slot_text(self.state.active),
            "pending_update_id": self.state.pending.map(|_| core::str::from_utf8(&self.state.update_id).unwrap_or("")),
            "phase": if self.trial() { "awaiting_confirmation" } else if self.state.pending.is_some() { "staged" } else { "stable" },
            "trial": self.trial(),
            "source": self.state.source,
            "last_outcome": outcome_text(self.state.outcome),
            "last_failure_reason": self.last_failure,
        })
    }

    fn protocol_state(&self) -> LoaderOtaState {
        LoaderOtaState {
            active_sha256: hex(self.state.digest(self.state.active)),
            running_sha256: hex(self.state.digest(self.slot)),
            pending_update_id: self.state.pending.map(|_| {
                core::str::from_utf8(&self.state.update_id)
                    .unwrap_or("")
                    .into()
            }),
            trial: self.trial(),
            source: self.trial().then_some(self.state.source),
            last_update_id: (self.state.update_id != [0; 36]).then(|| {
                core::str::from_utf8(&self.state.update_id)
                    .unwrap_or("")
                    .into()
            }),
            last_outcome: match self.state.outcome {
                Outcome::None => None,
                Outcome::RolledBack => Some(OtaOutcome::RolledBack),
                Outcome::LoadFailed => Some(OtaOutcome::Failed),
                Outcome::Confirmed => Some(OtaOutcome::Confirmed),
            },
        }
    }

    fn slot_path(slot: Slot) -> String {
        format!("{OTA_ROOT}/{}.EFI", slot_text(slot).to_ascii_uppercase())
    }

    fn slot_temporary_path(slot: Slot) -> String {
        format!("{OTA_ROOT}/{}.NEW", slot_text(slot).to_ascii_uppercase())
    }

    fn cleanup_temporary_slots() {
        for slot in [Slot::A, Slot::B] {
            let _ = fs::remove_file(&Self::slot_temporary_path(slot));
            // Remove the long-name temporary path used by the first implementation.
            let _ = fs::remove_file(&format!("{}.new", Self::slot_path(slot)));
        }
    }
}

fn current_loader_image() -> Option<Vec<u8>> {
    let (start, end) = someboot::loader_image_range()?;
    let len = end.checked_sub(start)?;
    if len == 0 || len > MAX_IMAGE_BYTES {
        return None;
    }
    let pointer = someboot::mem::phys_to_virt(start);
    // SAFETY: someboot records the firmware-owned image range before handing
    // control to axstd, and the allocation remains reserved for this loader.
    let mut image = unsafe { core::slice::from_raw_parts(pointer.cast_const(), len) }.to_vec();
    LoadedPe::canonicalize_loaded_image(&mut image).ok()?;
    Some(image)
}

impl OtaStorage for LaOta {
    type Error = &'static str;

    fn commit_state(&mut self, state: &State) -> Result<(), Self::Error> {
        self.commit_inner(state)
    }
}

fn slot_text(slot: Slot) -> &'static str {
    match slot {
        Slot::A => "a",
        Slot::B => "b",
    }
}

fn outcome_text(outcome: Outcome) -> Option<&'static str> {
    match outcome {
        Outcome::None => None,
        Outcome::RolledBack => Some("rolled_back"),
        Outcome::LoadFailed => Some("failed"),
        Outcome::Confirmed => Some("confirmed"),
    }
}
