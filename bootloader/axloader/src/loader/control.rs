extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
};
use core::time::Duration;

use axloader::{
    boot_offer::{BootManifest, BootManifestDecision, validate_boot_manifest},
    ota_state::Source,
};
use httpboot_protocol::{
    BootArch, BootFile, ImageFormat, LoaderDiscoveryProbe, LoaderPollRequest, LoaderPollResponse,
    LoaderStatusPhase, LoaderStatusReport, PROTOCOL_VERSION,
};

use super::{
    direct::Listener,
    http::{HttpClient, download_ota_image},
    network::NetworkInterface,
    ota::{OtaContext, decode_sha},
    smbios,
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    Network,
    Discovery,
    Http,
    InvalidOffer,
}

#[derive(Debug, Clone)]
pub struct BootOffer {
    pub board_id: String,
    pub session_id: String,
    pub boot_id: String,
    pub kernel_url: String,
    pub kernel_size: u64,
    pub kernel_sha256: String,
    pub image_format: ImageFormat,
    pub arch: BootArch,
    pub entry_symbol: Option<String>,
    pub initramfs: Option<BootFile>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NetworkBoot {
    pub interface: NetworkInterface,
    pub offer: BootOffer,
    control_base_url: String,
    registration_id: String,
    protocol_version: u16,
}

impl NetworkBoot {
    pub fn report_status(&self, status: LoaderStatusPhase) -> Result<(), ControlError> {
        let report = LoaderStatusReport {
            protocol_version: self.protocol_version,
            registration_id: self.registration_id.clone(),
            mac_address: self.interface.mac_address,
            session_id: self.offer.session_id.clone(),
            boot_id: self.offer.boot_id.clone(),
            status,
        };
        let url = endpoint(&self.control_base_url, "/api/v1/loaders/status");
        let mut client = HttpClient::new(self.interface.handle()).map_err(|error| {
            crate::logln!("loader_status_http_client_error: {error:?}");
            ControlError::Http
        })?;
        client.post_status(&url, &report).map_err(|error| {
            crate::logln!("loader_status_http_error: {error:?}");
            ControlError::Http
        })
    }
}

pub fn fetch_boot_offer(
    interface: NetworkInterface,
    failed_boot_id: Option<&str>,
    ota: &mut Option<OtaContext>,
    listener: &mut Option<Listener>,
) -> Result<NetworkBoot, ControlError> {
    crate::logln!(
        "network_ready: mac={} current_mac={} ip={}",
        interface.mac_address,
        interface.current_mac_address,
        interface.station_address
    );
    let probe = LoaderDiscoveryProbe {
        protocol_version: if ota.is_some() { 4 } else { PROTOCOL_VERSION },
        mac_address: interface.mac_address,
        current_mac_address: interface.current_mac_address,
        arch: target_arch(),
        loader_version: env!("CARGO_PKG_VERSION").into(),
    };
    let discovery = interface
        .discover_server(&probe, &mut || service_direct(listener, ota))
        .map_err(|error| {
            crate::logln!("loader_discovery_error: {error:?}");
            ControlError::Discovery
        })?;
    crate::logln!(
        "loader_server: id={} base={}",
        discovery.server_id,
        discovery.control_base_url
    );
    let poll_url = endpoint(&discovery.control_base_url, "/api/v1/loaders/poll");
    let hardware = smbios::hardware_info();

    loop {
        reopen_direct(&interface, listener, ota);
        service_direct(listener, ota);
        let request = LoaderPollRequest {
            protocol_version: if ota.is_some() { 4 } else { PROTOCOL_VERSION },
            registration_id: discovery.registration_id.clone(),
            mac_address: interface.mac_address,
            current_mac_address: interface.current_mac_address,
            ip_address: interface.station_address.to_string(),
            arch: target_arch(),
            loader_version: env!("CARGO_PKG_VERSION").into(),
            hardware: hardware.clone(),
        };
        let mut payload = serde_json::to_value(&request).map_err(|_| ControlError::Http)?;
        if let Some(ota) = ota.as_ref() {
            payload["ota"] = ota.poll_state();
        }
        // OVMF cannot reliably poll an HTTP child while a passive TCP4
        // listener is configured on the same controller. Reopen the listener
        // as soon as this bounded control exchange completes.
        drop(listener.take());
        let mut client = HttpClient::new(interface.handle()).map_err(|error| {
            crate::logln!("loader_poll_http_client_error: {error:?}");
            ControlError::Http
        })?;
        let response: serde_json::Value =
            client.post_json(&poll_url, &payload).map_err(|error| {
                crate::logln!("loader_poll_http_error: {error:?}");
                ControlError::Http
            })?;
        // UEFI keys OpenProtocol records by handle/agent/controller rather than by
        // Rust guard instance. Close this request before a boot response creates a
        // second HTTP child for the accepted status report.
        drop(client);
        match response.get("state").and_then(serde_json::Value::as_str) {
            Some("update") => {
                let ota = ota.as_mut().ok_or(ControlError::InvalidOffer)?;
                let id = response
                    .get("update_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ControlError::InvalidOffer)?;
                let path = response
                    .get("image_path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ControlError::InvalidOffer)?;
                let size = response
                    .get("image_size")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|size| usize::try_from(size).ok())
                    .ok_or(ControlError::InvalidOffer)?;
                let digest = response
                    .get("image_sha256")
                    .and_then(serde_json::Value::as_str)
                    .and_then(decode_sha)
                    .ok_or(ControlError::InvalidOffer)?;
                let id_bytes: [u8; 36] = id
                    .as_bytes()
                    .try_into()
                    .map_err(|_| ControlError::InvalidOffer)?;
                let (disk, mut writer) = ota
                    .start_update(size)
                    .map_err(|_| ControlError::InvalidOffer)?;
                report_ota(
                    &interface,
                    &discovery.control_base_url,
                    &discovery.registration_id,
                    id,
                    "downloading",
                    None,
                    None,
                );
                let result = download_ota_image(
                    interface.handle(),
                    &endpoint(&discovery.control_base_url, path),
                    size,
                    |bytes| writer.write(bytes),
                );
                if result.is_err()
                    || ota
                        .finish_update(disk, writer, digest, id_bytes, Source::Server, None)
                        .is_err()
                {
                    report_ota(
                        &interface,
                        &discovery.control_base_url,
                        &discovery.registration_id,
                        id,
                        "failed",
                        Some("download or image verification failed"),
                        None,
                    );
                    return Err(ControlError::InvalidOffer);
                }
                report_ota(
                    &interface,
                    &discovery.control_base_url,
                    &discovery.registration_id,
                    id,
                    "staged",
                    None,
                    None,
                );
                drop(listener.take());
                uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None);
            }
            Some("confirm_update") => {
                let ota = ota.as_mut().ok_or(ControlError::InvalidOffer)?;
                let id = response
                    .get("update_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ControlError::InvalidOffer)?;
                ota.confirm(id, Source::Server)
                    .map_err(|_| ControlError::InvalidOffer)?;
                let committed = ota.poll_state();
                report_ota(
                    &interface,
                    &discovery.control_base_url,
                    &discovery.registration_id,
                    id,
                    "succeeded",
                    None,
                    committed["active_sha256"].as_str(),
                );
                continue;
            }
            _ => {}
        }
        let response: LoaderPollResponse =
            serde_json::from_value(response).map_err(|_| ControlError::InvalidOffer)?;
        match response {
            LoaderPollResponse::Unbound => crate::logln!("loader_state: unbound"),
            LoaderPollResponse::BoundIdle { board_id } => {
                crate::logln!("loader_state: bound_idle board_id={board_id}")
            }
            LoaderPollResponse::Reject {
                code,
                message,
                retry_after_ms,
            } => {
                crate::logln!("loader_state: reject code={code} message={message}");
                reopen_direct(&interface, listener, ota);
                service_wait(
                    Duration::from_millis(
                        retry_after_ms.unwrap_or(POLL_INTERVAL.as_millis() as u64),
                    ),
                    listener,
                    ota,
                );
                continue;
            }
            LoaderPollResponse::Boot {
                board_id,
                session_id,
                boot_id,
                kernel_path,
                kernel_size,
                kernel_sha256,
                arch,
                image_format,
                entry_symbol,
                initramfs,
                cmdline,
            } => {
                let kernel_url = endpoint(&discovery.control_base_url, &kernel_path);
                match validate_boot_manifest(
                    BootManifest {
                        boot_id: &boot_id,
                        kernel_url: &kernel_url,
                        kernel_size,
                        kernel_sha256: &kernel_sha256,
                        arch,
                        image_format,
                    },
                    failed_boot_id,
                    target_arch(),
                ) {
                    BootManifestDecision::Accept => {}
                    BootManifestDecision::WaitForNewBoot => {
                        crate::logln!("loader_state: waiting_for_new_boot boot_id={boot_id}");
                        reopen_direct(&interface, listener, ota);
                        service_wait(POLL_INTERVAL, listener, ota);
                        continue;
                    }
                    BootManifestDecision::Reject => return Err(ControlError::InvalidOffer),
                }
                if ota.as_ref().is_some_and(OtaContext::trial) {
                    crate::logln!("ota_trial_waiting_for_confirmation");
                    reopen_direct(&interface, listener, ota);
                    service_wait(POLL_INTERVAL, listener, ota);
                    continue;
                }
                let boot = NetworkBoot {
                    interface,
                    offer: BootOffer {
                        board_id,
                        session_id,
                        boot_id,
                        kernel_url,
                        kernel_size,
                        kernel_sha256,
                        image_format,
                        arch,
                        entry_symbol,
                        initramfs: initramfs.map(|file| BootFile {
                            path: endpoint(&discovery.control_base_url, &file.path),
                            ..file
                        }),
                        cmdline,
                    },
                    control_base_url: discovery.control_base_url,
                    registration_id: discovery.registration_id,
                    protocol_version: if ota.is_some() { 4 } else { PROTOCOL_VERSION },
                };
                drop(listener.take());
                boot.report_status(LoaderStatusPhase::Accepted)?;
                return Ok(boot);
            }
        }
        reopen_direct(&interface, listener, ota);
        service_wait(POLL_INTERVAL, listener, ota);
    }
}

pub(super) fn reopen_direct(
    interface: &NetworkInterface,
    listener: &mut Option<Listener>,
    ota: &Option<OtaContext>,
) {
    if ota.is_some() && listener.is_none() {
        match Listener::open(interface.handle()) {
            Ok(service) => *listener = Some(service),
            Err(error) => crate::logln!("ota_direct_reopen_error: {error:?}"),
        }
    }
}

pub fn service_direct(listener: &mut Option<Listener>, ota: &mut Option<OtaContext>) {
    let reset = match (listener.as_mut(), ota.as_mut()) {
        (Some(listener), Some(ota)) => listener.poll(ota),
        _ => false,
    };
    if reset {
        crate::logln!("ota_direct_staged_reboot");
        drop(listener.take());
        uefi::runtime::reset(uefi::runtime::ResetType::COLD, uefi::Status::SUCCESS, None);
    }
}

pub fn service_wait(
    duration: Duration,
    listener: &mut Option<Listener>,
    ota: &mut Option<OtaContext>,
) {
    let mut elapsed = Duration::ZERO;
    while elapsed < duration {
        service_direct(listener, ota);
        let step = (duration - elapsed).min(Duration::from_millis(20));
        uefi::boot::stall(step);
        elapsed += step;
    }
}

fn report_ota(
    interface: &NetworkInterface,
    base: &str,
    registration_id: &str,
    id: &str,
    phase: &str,
    error: Option<&str>,
    active_sha256: Option<&str>,
) {
    let body = serde_json::json!({
        "protocol_version": 4, "registration_id": registration_id,
        "mac_address": interface.mac_address, "update_id": id,
        "phase": phase, "error": error, "active_sha256": active_sha256,
    });
    if let Ok(mut client) = HttpClient::new(interface.handle()) {
        if let Err(err) = client.post_status(&endpoint(base, "/api/v1/loaders/ota-status"), &body) {
            crate::logln!("ota_status_report_error: {err:?}");
        }
    }
}

fn endpoint(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

#[cfg(target_arch = "x86_64")]
const fn target_arch() -> BootArch {
    BootArch::X86_64
}

#[cfg(target_arch = "aarch64")]
const fn target_arch() -> BootArch {
    BootArch::Aarch64
}

#[cfg(target_arch = "riscv64")]
const fn target_arch() -> BootArch {
    BootArch::Riscv64
}

#[cfg(target_arch = "loongarch64")]
const fn target_arch() -> BootArch {
    BootArch::Loongarch64
}
