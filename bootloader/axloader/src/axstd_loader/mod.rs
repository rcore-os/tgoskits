//! LoongArch ArceOS EFI transport, control loop and resource plan.
//! Image ownership and terminal entry live in `image`; persistent slot state
//! lives in `ota`. The control loop drops its sockets before entering a target.

use std::{
    format,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket},
    println,
    string::{String, ToString},
    vec::Vec,
};

use axloader::{
    boot_job::{BootJobStore, FileKind},
    http_control::{
        MAX_HEADER_BYTES, MAX_IMAGE_BYTES, RequestHead, Route, error_status, parse_head,
        parse_update_id, route as control_route, validate_body_length,
    },
    integrity,
    ota::Outcome,
    target::LOONGARCH64_EFI,
};
use httpboot_protocol::{
    BootArch, DEVICE_PROTOCOL_VERSION, DeviceBootJob, LoaderAnnouncement, LoaderDeviceStatus,
    LoaderHardwareInfo, MAX_DISCOVERY_DATAGRAM_BYTES, MAX_HTTP_BOOT_INITRAMFS_BYTES, MacAddress,
    OtaSource, SerialParameters,
};
use sha2::{Digest, Sha256};
use someboot::boot_resources::{Placement, ResourceId, ResourceRequest};

const IMAGE_RESOURCE: ResourceId = ResourceId(1);
const ARCHIVE_RESOURCE: ResourceId = ResourceId(2);
const FIRMWARE_RESOURCE: ResourceId = ResourceId(4);
const SLOT_RESOURCE: ResourceId = ResourceId(3);
someboot::declare_boot_resources! {
    ResourceRequest::new(IMAGE_RESOURCE, 32 * 1024 * 1024, 4096, Placement::At(0x20_0000)),
    ResourceRequest::new(ARCHIVE_RESOURCE, 256 * 1024 * 1024, 4096, Placement::Anywhere),
    ResourceRequest::new(SLOT_RESOURCE, 32 * 1024 * 1024, 4096, Placement::Anywhere),
    ResourceRequest::new(FIRMWARE_RESOURCE, MAX_IMAGE_BYTES as u64, 4096, Placement::Anywhere)
        .with_initial_contents(someboot::boot_resources::InitialContents::FirmwareBootImage),
}

const HTTP_PORT: u16 = 2999;
const DISCOVERY_PORT: u16 = 2998;
mod image;
mod ota;

use image::{BootExecution, LoadedElf, PublishedPayload};
use ota::{LaOta, OtaStageError, firmware_boot_image, inherited_ota_slot};

pub fn run() {
    println!("axloader: LoongArch ArceOS EFI frontend");
    report_memory_image();
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
                // Close application sockets before the runtime joins network
                // workers and confirms that device DMA has stopped.
                drop(stream);
                drop(listener);
                execution.handoff();
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

/// Report after the runtime console is active: firmware/early-map diagnostics
/// can be absent from CI serial capture. A cold record starts a new lifetime.
fn report_memory_image() {
    let image = someboot::handoff::current_image().expect("EFI loader image context");
    let inherited = someboot::handoff::previous_image().is_some();
    let record = format!(
        "axloader_memory inherited={} start={:#x} bytes={:#x}\n",
        u8::from(inherited),
        image.start,
        image.len
    );
    let output = ax_std::os::arceos::api::modules::ax_runtime::console::output()
        .unwrap_or_else(|error| panic!("loader memory console unavailable: {error}"));
    output
        .write_text_all(record.as_bytes())
        .unwrap_or_else(|error| panic!("loader memory report failed: {error}"));
    output
        .drain()
        .unwrap_or_else(|error| panic!("loader memory report drain failed: {error}"));
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
    head: RequestHead,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.header(name)
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
    Ok(HttpRequest { head, body })
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
    let selected_route = control_route(&request.head.method, &request.head.path);
    if let Err(error) =
        selected_route.check_admission(request.header("x-boot-epoch"), &server.epoch, ota.trial())
    {
        return Ok(Response::json(error.status(), error.body(), Action::None));
    }
    if selected_route == Route::DeviceStatus {
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
    if selected_route == Route::SerialContinue {
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
    if let Route::SerialBindingRevoke { id: binding_id } = selected_route {
        let result = server.serial.revoke(binding_id);
        return Ok(Response::empty(if result.is_ok() {
            "200 OK"
        } else {
            "409 Conflict"
        }));
    }
    if selected_route == Route::OtaStatus {
        return Ok(Response::json(
            "200 OK",
            serde_json::to_vec(&ota.status_json()).map_err(|error| error.to_string())?,
            Action::None,
        ));
    }
    if selected_route == Route::OtaConfirm {
        let value: serde_json::Value = serde_json::from_slice(&request.body)
            .map_err(|_| "invalid OTA confirmation".to_string())?;
        let update_id = value
            .get("update_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "missing OTA update id".to_string())?;
        if parse_update_id(update_id).is_none() {
            return Err("invalid OTA update id".to_string());
        }
        let source = request.head.ota_source()?;
        if ota.confirm(update_id, source).is_err() {
            return Ok(Response::json("409 Conflict", b"{}", Action::None));
        }
        return Ok(Response::json(
            "200 OK",
            serde_json::to_vec(&ota.status_json()).map_err(|error| error.to_string())?,
            Action::None,
        ));
    }
    if selected_route == Route::OtaImage {
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
        let Some(expected) = request.head.ota_image_sha256() else {
            return Ok(Response::json("400 Bad Request", b"{}", Action::None));
        };
        if request.body.is_empty() || request.body.len() > MAX_IMAGE_BYTES {
            return Ok(Response::json("413 Payload Too Large", b"{}", Action::None));
        }
        let source = request.head.ota_source()?;
        let version = request.head.ota_image_version()?;
        let update_id = match source {
            OtaSource::Server => request
                .head
                .ota_update_id()
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
    if selected_route == Route::BootJobCreate {
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
    if let Route::BootJobStatus { id } = selected_route {
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
    if let Route::BootJobCancel { id } = selected_route {
        let status = if server.cancel(id).is_ok() {
            "204 No Content"
        } else {
            "404 Not Found"
        };
        return Ok(Response::empty(status));
    }
    if let Route::BootJobUpload { id, kind } = selected_route {
        let descriptor = server
            .descriptor(id, kind)
            .map_err(|error| error.to_string())?;
        if descriptor.size as usize != request.body.len()
            || request.head.ota_image_sha256() != integrity::decode_sha256(&descriptor.sha256)
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
    if let Route::BootJobStart { id } = selected_route {
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
        let elf = LoadedElf::load(
            kernel,
            LOONGARCH64_EFI,
            job.manifest.entry_symbol.as_deref(),
        )
        .map_err(|_| "kernel_load_failed")?;
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
