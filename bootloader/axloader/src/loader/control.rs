extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
};
use core::time::Duration;

use axloader::boot_offer::{BootManifest, BootManifestDecision, validate_boot_manifest};
use httpboot_protocol::{
    BootArch, ImageFormat, LoaderDiscoveryProbe, LoaderPollRequest, LoaderPollResponse,
    LoaderStatusPhase, LoaderStatusReport, PROTOCOL_VERSION,
};

use super::{http::HttpClient, network::NetworkInterface, smbios};

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
}

#[derive(Debug, Clone)]
pub struct NetworkBoot {
    pub interface: NetworkInterface,
    pub offer: BootOffer,
    control_base_url: String,
    registration_id: String,
}

impl NetworkBoot {
    pub fn report_status(&self, status: LoaderStatusPhase) -> Result<(), ControlError> {
        let report = LoaderStatusReport {
            protocol_version: PROTOCOL_VERSION,
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

pub fn fetch_boot_offer(failed_boot_id: Option<&str>) -> Result<NetworkBoot, ControlError> {
    let interface = NetworkInterface::select().map_err(|_| ControlError::Network)?;
    crate::logln!(
        "network_ready: mac={} current_mac={} ip={}",
        interface.mac_address,
        interface.current_mac_address,
        interface.station_address
    );
    let probe = LoaderDiscoveryProbe {
        protocol_version: PROTOCOL_VERSION,
        mac_address: interface.mac_address,
        current_mac_address: interface.current_mac_address,
        arch: target_arch(),
        loader_version: env!("CARGO_PKG_VERSION").into(),
    };
    let discovery = interface.discover_server(&probe).map_err(|error| {
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
        let request = LoaderPollRequest {
            protocol_version: PROTOCOL_VERSION,
            registration_id: discovery.registration_id.clone(),
            mac_address: interface.mac_address,
            current_mac_address: interface.current_mac_address,
            ip_address: interface.station_address.to_string(),
            arch: target_arch(),
            loader_version: env!("CARGO_PKG_VERSION").into(),
            hardware: hardware.clone(),
        };
        let mut client = HttpClient::new(interface.handle()).map_err(|error| {
            crate::logln!("loader_poll_http_client_error: {error:?}");
            ControlError::Http
        })?;
        let response: LoaderPollResponse =
            client.post_json(&poll_url, &request).map_err(|error| {
                crate::logln!("loader_poll_http_error: {error:?}");
                ControlError::Http
            })?;
        // UEFI keys OpenProtocol records by handle/agent/controller rather than by
        // Rust guard instance. Close this request before a boot response creates a
        // second HTTP child for the accepted status report.
        drop(client);
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
                uefi::boot::stall(Duration::from_millis(
                    retry_after_ms.unwrap_or(POLL_INTERVAL.as_millis() as u64),
                ));
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
                        uefi::boot::stall(POLL_INTERVAL);
                        continue;
                    }
                    BootManifestDecision::Reject => return Err(ControlError::InvalidOffer),
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
                    },
                    control_base_url: discovery.control_base_url,
                    registration_id: discovery.registration_id,
                };
                boot.report_status(LoaderStatusPhase::Accepted)?;
                return Ok(boot);
            }
        }
        uefi::boot::stall(POLL_INTERVAL);
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
