//! LoongArch ArceOS EFI loader frontend.

use core::{mem, ptr::NonNull};
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
    efi_image,
    http_control::{
        MAX_HEADER_BYTES, MAX_IMAGE_BYTES, error_status, parse_head, parse_update_id,
        valid_image_version, validate_body_length,
    },
    integrity,
    ota::{self, Outcome, Slot, State},
    target::LOONGARCH64_EFI,
};
use httpboot_protocol::{
    BootArch, DEVICE_PROTOCOL_VERSION, DeviceBootJob, LoaderAnnouncement, LoaderDeviceStatus,
    LoaderHardwareInfo, LoaderOtaState, MAX_DISCOVERY_DATAGRAM_BYTES,
    MAX_HTTP_BOOT_INITRAMFS_BYTES, MacAddress, OtaOutcome, OtaSource, SerialParameters,
};
use sha2::{Digest, Sha256};
use someboot::{
    boot_resources::{self, Allocation, PhysicalRange, Placement, ResourceId, ResourceRequest},
    handoff::{ApplicationMetadata, HandoffSpec, PreparedHandoff},
};

const IMAGE_RESOURCE: ResourceId = ResourceId(1);
const ARCHIVE_RESOURCE: ResourceId = ResourceId(2);
const FIRMWARE_RESOURCE: ResourceId = ResourceId(4);
const SLOT_RESOURCE: ResourceId = ResourceId(3);
const OTA_METADATA_ID: [u8; 16] = *b"AXLOADER-OTA-V1!";
someboot::declare_boot_resources! {
    ResourceRequest::new(IMAGE_RESOURCE, 32 * 1024 * 1024, 4096, Placement::At(0x20_0000)),
    ResourceRequest::new(ARCHIVE_RESOURCE, 256 * 1024 * 1024, 4096, Placement::Anywhere),
    ResourceRequest::new(SLOT_RESOURCE, 32 * 1024 * 1024, 4096, Placement::Anywhere),
    ResourceRequest::new(FIRMWARE_RESOURCE, MAX_IMAGE_BYTES as u64, 4096, Placement::Anywhere)
        .with_initial_contents(someboot::boot_resources::InitialContents::FirmwareBootImage),
}

const HTTP_PORT: u16 = 2999;
const DISCOVERY_PORT: u16 = 2998;
const OTA_ROOT: &str = "axloader";
const ELF_PF_X: u32 = 1;
pub fn run() {
    println!("axloader: LoongArch ArceOS EFI frontend");
    let current_image = firmware_boot_image()
        .unwrap_or_else(|| panic!("ota_bootstrap: current loader image unavailable"));
    let mut current_digest: [u8; 32] = Sha256::digest(&current_image).into();
    let mut ota = LaOta::open(&current_image);
    let inherited_slot =
        inherited_ota_slot(&ota.state).unwrap_or_else(|error| panic!("ota_handoff: {error}"));
    // The parent commits the generation immediately before entering an
    // inherited loader. Treat that handoff as the same boot attempt instead
    // of advancing the durable identity a second time in the child.
    let mut boot_advanced = inherited_slot.is_some();
    if let Some(slot) = inherited_slot {
        ota.set_running_slot(slot)
            .unwrap_or_else(|error| panic!("ota_slot: {error}"));
        current_digest = *ota.state.digest(ota.slot);
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
                    (execution.elf.load_end - execution.elf.load_addr) / 4096
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

impl Interface {
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

fn error_json(error: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "error": error })).unwrap_or_else(|_| b"{}".to_vec())
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 2048];
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
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
            if header_end > MAX_HEADER_BYTES {
                return Err("request header too large".into());
            }
            break header_end;
        }
    };
    let (head, parsed_header_end) = parse_head(&bytes).map_err(str::to_string)?;
    debug_assert_eq!(parsed_header_end, header_end);
    let method = head.method.as_str();
    let path = head.path.as_str();
    let length = head.content_length;
    let mut body = bytes[header_end..].to_vec();
    validate_body_length(
        method,
        path,
        length,
        body.len(),
        MAX_HTTP_BOOT_INITRAMFS_BYTES,
    )
    .map_err(str::to_string)?;
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
        headers: head.headers,
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
        if let Err(error) = ota.stage_image(&request.body, expected, update_id, version, source) {
            let (status, body): (_, &[u8]) = match error {
                OtaStageError::InvalidImage => (
                    "422 Unprocessable Content",
                    br#"{"error":"image_digest_or_efi_validation_failed"}"#,
                ),
                OtaStageError::Conflict => ("409 Conflict", br#"{"error":"ota_state_conflict"}"#),
                OtaStageError::Storage(reason) => {
                    println!("ota_stage_storage_failed: {reason}");
                    ("409 Conflict", br#"{"error":"ota_storage_failed"}"#)
                }
            };
            return Ok(Response::json(status, body, Action::None));
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
    _allocation: Allocation,
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
        let allocation =
            reserve_image(IMAGE_RESOURCE, page_count).map_err(|_| ElfError::AddressConflict)?;
        let target_phys = allocation.range().start as usize;
        let target_end = match target_phys.checked_add(load_end as usize - link_addr as usize) {
            Some(end) => end,
            None => {
                return Err(ElfError::AddressConflict);
            }
        };
        let (loader_start, loader_end) = match someboot::handoff::current_image() {
            Some(range) => (range.start as usize, range.end().unwrap() as usize),
            None => {
                return Err(ElfError::AddressConflict);
            }
        };
        if target_phys < loader_end && loader_start < target_end {
            return Err(ElfError::AddressConflict);
        }
        if spec
            .link_address
            .is_some_and(|expected| expected != link_addr)
            || spec
                .load_address
                .is_some_and(|expected| expected != target_phys as u64)
        {
            return Err(ElfError::AddressConflict);
        }
        let target = match NonNull::new(someboot::mem::phys_to_virt(target_phys)) {
            Some(target) => target,
            None => {
                return Err(ElfError::Allocation);
            }
        };
        let allocation_bytes = match page_count.checked_mul(4096) {
            Some(bytes) => bytes,
            None => {
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
            return Err(ElfError::Invalid);
        }
        someboot::handoff::flush_instruction_cache();
        Ok(Self {
            entry_point: someboot::mem::phys_to_virt(target_phys + (entry - link_addr) as usize)
                as u64,
            load_addr: target_phys as u64,
            load_end: target_end as u64,
            _allocation: allocation,
        })
    }
}

#[derive(Debug)]
struct LoadedPe {
    entry_point: u64,
    load_addr: usize,
    load_end: usize,
    _allocation: Allocation,
}

impl LoadedPe {
    fn validate(image: &[u8]) -> Result<(), &'static str> {
        let layout = efi_image::layout(image)?;
        layout.validate(image.len())
    }

    fn load(image: &[u8]) -> Result<Self, &'static str> {
        let layout = efi_image::layout(image)?;
        layout.validate(image.len())?;
        let page_count = layout.image_size.div_ceil(4096);
        let allocation = reserve_image(SLOT_RESOURCE, page_count)?;
        let load_addr = allocation.range().start as usize;
        let target = someboot::mem::phys_to_virt(load_addr);
        // SAFETY: the slot pool is reserved by someboot and `image_size` stays
        // inside the fixed allocation returned above.
        unsafe {
            core::ptr::write_bytes(target, 0, page_count * 4096);
            core::ptr::copy_nonoverlapping(image.as_ptr(), target, layout.headers);
        }
        for section in layout.sections {
            // SAFETY: `efi_image::Layout::validate` checked each section's virtual range against
            // `image_size`, and `target` points to the allocation of
            // `page_count * 4096` bytes covering that complete image.
            let destination = unsafe { target.add(section.virtual_address) };
            if section.raw_size != 0 {
                // SAFETY: `efi_image::Layout::validate` checked the raw range and the destination
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
        someboot::handoff::flush_instruction_cache();
        Ok(Self {
            entry_point: someboot::mem::phys_to_virt(load_addr + layout.entry) as u64,
            load_addr,
            load_end: load_addr + page_count * 4096,
            _allocation: allocation,
        })
    }
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

fn reserve_image(id: ResourceId, pages: usize) -> Result<Allocation, &'static str> {
    let bytes = pages
        .checked_mul(4096)
        .and_then(|bytes| bytes.checked_add(0x1f_ffff))
        .map(|bytes| bytes & !0x1f_ffff)
        .ok_or("image reservation overflows")?;
    boot_resources::allocate(id, bytes as u64, 0x20_0000).map_err(|_| "image resource unavailable")
}

struct PublishedPayload(PreparedHandoff);
impl PublishedPayload {
    fn new(
        initramfs: Option<&[u8]>,
        cmdline: Option<&str>,
        target_image: Option<(usize, usize)>,
    ) -> Result<Self, &'static str> {
        Self::with_metadata(initramfs, cmdline, target_image, None)
    }
    fn with_metadata(
        initramfs: Option<&[u8]>,
        cmdline: Option<&str>,
        target_image: Option<(usize, usize)>,
        metadata: Option<ApplicationMetadata<'_>>,
    ) -> Result<Self, &'static str> {
        let (start, end) = target_image.ok_or("missing target image")?;
        let archive = initramfs
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| {
                boot_resources::allocate(ARCHIVE_RESOURCE, bytes.len() as u64, 4096)
                    .map_err(|_| "archive resource unavailable")
            })
            .transpose()?;
        PreparedHandoff::prepare(
            HandoffSpec {
                image: PhysicalRange::new(start as u64, (end - start) as u64),
                cmdline,
                initramfs,
                metadata,
            },
            archive,
        )
        .map(Self)
        .map_err(|_| "EFI handoff preparation failed")
    }
}

struct BootExecution {
    elf: LoadedElf,
    payload: PublishedPayload,
}
impl BootExecution {
    fn handoff(self) -> Result<(), someboot::handoff::HandoffError> {
        // SAFETY: the loader validated the EFI symbol and executable segment;
        // the image and payload owners stay live and network services are closed.
        unsafe { self.payload.0.enter(self.elf.entry_point) }
    }
}

fn inherited_ota_slot(state: &State) -> Result<Option<u8>, &'static str> {
    let Some(metadata) = someboot::handoff::inherited_metadata() else {
        return Ok(None);
    };
    if metadata.id != OTA_METADATA_ID {
        return Ok(None);
    }
    if metadata.version != 1 {
        return Err("unsupported OTA metadata version");
    }
    let metadata = ota::handoff::OtaHandoff::decode(metadata.bytes)?;
    if metadata.generation != state.generation {
        return Err("inherited OTA generation mismatch");
    }
    Ok(Some(metadata.slot as u8))
}

#[derive(Debug)]
enum OtaStageError {
    InvalidImage,
    Conflict,
    Storage(&'static str),
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
        let inherited = someboot::handoff::inherited_metadata().is_some();
        let (state, index) =
            match Self::load_state().unwrap_or_else(|error| panic!("ota_bootstrap: {error}")) {
                Some(state) => state,
                None => {
                    assert!(!inherited, "inherited OTA state is missing");
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
        let bytes = ota::handoff::OtaHandoff {
            slot,
            generation: self.state.generation,
        }
        .encode();
        let payload = PublishedPayload::with_metadata(
            None,
            someboot::cmdline(),
            Some((loaded.load_addr, loaded.load_end)),
            Some(ApplicationMetadata {
                id: OTA_METADATA_ID,
                version: 1,
                bytes: &bytes,
            }),
        )?;
        // SAFETY: validated EFI PE entry, with both owners retained for the call.
        unsafe { payload.0.enter(loaded.entry_point) }.map_err(|_| "OTA slot EFI entry failed")?;
        Err("OTA slot returned successfully")
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
    ) -> Result<(), OtaStageError> {
        if self.state.pending.is_some() {
            return Err(OtaStageError::Conflict);
        }
        if Sha256::digest(image).as_slice() != digest {
            return Err(OtaStageError::InvalidImage);
        }
        // Validate the image before replacing the inactive slot. A failed
        // upload must not leave an unbootable candidate behind.
        LoadedPe::validate(image).map_err(|_| OtaStageError::InvalidImage)?;
        let target = self.state.active.other();
        let path = Self::slot_path(target);
        let temporary = Self::slot_temporary_path(target);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|_| OtaStageError::Storage("OTA image create failed"))?;
        file.write_all(image)
            .map_err(|_| OtaStageError::Storage("OTA image write failed"))?;
        file.flush()
            .map_err(|_| OtaStageError::Storage("OTA image flush failed"))?;
        drop(file);
        // Keep the replacement name within FAT's short-name format. The state is
        // committed only after readback validation, so an interrupted replacement
        // remains a rejected inactive candidate on the next boot.
        let _ = fs::remove_file(&path);
        fs::rename(&temporary, &path)
            .map_err(|_| OtaStageError::Storage("OTA image commit failed"))?;
        let readback =
            fs::read(&path).map_err(|_| OtaStageError::Storage("OTA image readback failed"))?;
        if Sha256::digest(&readback).as_slice() != digest || LoadedPe::validate(&readback).is_err()
        {
            return Err(OtaStageError::Storage("OTA image readback mismatch"));
        }
        let next = self
            .state
            .stage_named(digest, update_id, source, version)
            .map_err(|_| OtaStageError::Conflict)?;
        self.commit(&next).map_err(OtaStageError::Storage)
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

fn firmware_boot_image() -> Option<Vec<u8>> {
    // Preserve the original file before EBS. Runtime data and relocated pointers
    // cannot be used to reconstruct a restartable EFI application.
    let image = someboot::boot_resources::initial_contents(FIRMWARE_RESOURCE)?;
    LoadedPe::validate(image).ok()?;
    Some(image.to_vec())
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
