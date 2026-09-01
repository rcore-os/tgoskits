//! FDT resources and board policy for the CV181x AIC8800 attachment.

use alloc::{format, string::String};
use core::time::Duration;

use aic8800::AicRdifOptions;
use rd_net::{NetIrqSourceId, WifiLinkPolicy, WifiTransaction};
use rdrive::{probe::OnProbeError, register::FdtInfo};

use crate::cv181x::{
    CRG_MIN_MMIO_SIZE, MmioRegion, RTCSYS_CTRL_MIN_MMIO_SIZE, RTCSYS_IO_MIN_MMIO_SIZE,
    SDHCI_MIN_MMIO_SIZE, SYSCON_MIN_MMIO_SIZE, controller_region, fdt_string, fdt_u32, host_config,
    required_region,
};

const CONTROLLER_REG_NAME: &str = "sdio";
const SYSCON_REG_NAME: &str = "syscon";
const CRG_REG_NAME: &str = "crg";
const RTCSYS_CTRL_REG_NAME: &str = "rtcsys-ctrl";
const RTCSYS_IO_REG_NAME: &str = "rtcsys-io";

const SYSCON_PHANDLE: &str = "cvitek,syscon";
const CRG_PHANDLE: &str = "cvitek,crg";
const RTCSYS_CTRL_PHANDLE: &str = "cvitek,rtcsys-ctrl";
const RTCSYS_IO_PHANDLE: &str = "cvitek,rtcsys-io";

/// Fully translated platform input consumed by the probe orchestration.
pub(super) struct AicFdtProfile {
    pub(super) controller: MmioRegion,
    pub(super) syscon: MmioRegion,
    pub(super) crg: MmioRegion,
    pub(super) rtcsys_ctrl: MmioRegion,
    pub(super) rtcsys_io: MmioRegion,
    pub(super) host_config: cv181x_sdhci::Cv181xConfig,
    pub(super) dma_address_mask: u64,
    pub(super) options: AicRdifOptions,
}

impl AicFdtProfile {
    pub(super) fn from_info(
        info: &FdtInfo<'_>,
        prepared_source_hz: Option<u64>,
    ) -> Result<Self, OnProbeError> {
        let controller = controller_region(info, CONTROLLER_REG_NAME, SDHCI_MIN_MMIO_SIZE)?;
        let syscon = required_region(info, SYSCON_REG_NAME, SYSCON_PHANDLE, SYSCON_MIN_MMIO_SIZE)?;
        let crg = required_region(info, CRG_REG_NAME, CRG_PHANDLE, CRG_MIN_MMIO_SIZE)?;
        let rtcsys_ctrl = required_region(
            info,
            RTCSYS_CTRL_REG_NAME,
            RTCSYS_CTRL_PHANDLE,
            RTCSYS_CTRL_MIN_MMIO_SIZE,
        )?;
        let rtcsys_io = required_region(
            info,
            RTCSYS_IO_REG_NAME,
            RTCSYS_IO_PHANDLE,
            RTCSYS_IO_MIN_MMIO_SIZE,
        )?;
        let mut options =
            AicRdifOptions::new(NetIrqSourceId::new(0)).with_startup_delay(startup_delay(info));
        if let Some(timeout) = duration_millis(info, "aic,startup-timeout-ms") {
            options = options.with_startup_timeout(timeout);
        }
        if let Some(timeout) = duration_millis(info, "aic,control-timeout-ms") {
            options = options.with_control_timeout(timeout);
        }
        if let Some(queue_size) = fdt_usize(info, "aic,queue-size")? {
            options.queue_size = queue_size;
        }
        if let Some(frame_size) = fdt_usize(info, "aic,max-frame-size")? {
            options.frame_size = frame_size;
        }
        if let Some(transaction) = startup_transaction(info)? {
            options = options.with_startup_transaction(transaction);
        }

        Ok(Self {
            controller,
            syscon,
            crg,
            rtcsys_ctrl,
            rtcsys_io,
            host_config: host_config(info, prepared_source_hz),
            dma_address_mask: dma_address_mask(info)?,
            options,
        })
    }
}

fn startup_delay(info: &FdtInfo<'_>) -> Duration {
    fdt_u32(info, "post-power-on-delay-ms")
        .map(|milliseconds| Duration::from_millis(u64::from(milliseconds)))
        .unwrap_or(cv181x_sdhci::CV181X_SDIO1_RESET_SETTLE)
}

fn duration_millis(info: &FdtInfo<'_>, property: &str) -> Option<Duration> {
    fdt_u32(info, property)
        .map(u64::from)
        .map(Duration::from_millis)
}

fn fdt_usize(info: &FdtInfo<'_>, property: &str) -> Result<Option<usize>, OnProbeError> {
    fdt_u32(info, property)
        .map(|value| {
            usize::try_from(value).map_err(|_| {
                OnProbeError::other(format!(
                    "[{}] property '{property}' does not fit usize",
                    info.node.name()
                ))
            })
        })
        .transpose()
}

fn dma_address_mask(info: &FdtInfo<'_>) -> Result<u64, OnProbeError> {
    let bits = fdt_u32(info, "dma-address-bits").unwrap_or(64);
    match bits {
        1..=63 => Ok((1_u64 << bits) - 1),
        64 => Ok(u64::MAX),
        _ => Err(OnProbeError::other(format!(
            "[{}] dma-address-bits must be in 1..=64",
            info.node.name()
        ))),
    }
}

fn startup_transaction(info: &FdtInfo<'_>) -> Result<Option<WifiTransaction>, OnProbeError> {
    let build_transaction = super::startup_config::transaction().map_err(|error| {
        OnProbeError::other(format!(
            "[{}] invalid compile-time Wi-Fi startup configuration: {error}",
            info.node.name()
        ))
    })?;
    let firmware_transaction = fdt_startup_transaction(info)?;
    match (build_transaction, firmware_transaction) {
        (Some(_), Some(_)) => Err(OnProbeError::other(format!(
            "[{}] compile-time station policy conflicts with FDT startup policy",
            info.node.name()
        ))),
        (Some(transaction), None) | (None, Some(transaction)) => Ok(Some(transaction)),
        (None, None) => Ok(None),
    }
}

fn fdt_startup_transaction(info: &FdtInfo<'_>) -> Result<Option<WifiTransaction>, OnProbeError> {
    let Some(mode) = fdt_string(info, "aic,startup-mode") else {
        return Ok(None);
    };
    match mode.as_str() {
        "none" => Ok(None),
        "access-point" => access_point_transaction(info).map(Some),
        other => Err(OnProbeError::other(format!(
            "[{}] unsupported aic,startup-mode '{other}'",
            info.node.name()
        ))),
    }
}

fn access_point_transaction(info: &FdtInfo<'_>) -> Result<WifiTransaction, OnProbeError> {
    let ssid = required_string(info, "aic,ap-ssid")?.into_bytes();
    let channel = required_u8(info, "aic,ap-channel")?;
    if !(1..=14).contains(&channel) {
        return Err(OnProbeError::other(format!(
            "[{}] aic,ap-channel must be in 1..=14",
            info.node.name()
        )));
    }
    let prefix_len = required_u8(info, "aic,ap-prefix-length")?;
    if prefix_len > 32 {
        return Err(OnProbeError::other(format!(
            "[{}] aic,ap-prefix-length must not exceed 32",
            info.node.name()
        )));
    }
    let ip = required_ipv4(info, "aic,ap-ipv4")?;
    let dhcp_server_client_ip = fdt_u32(info, "aic,dhcp-client-ipv4").map(u32::to_be_bytes);
    Ok(WifiTransaction::open_access_point(
        ssid,
        channel,
        WifiLinkPolicy {
            ip,
            prefix_len,
            dhcp_server_client_ip,
        },
    ))
}

fn required_string(info: &FdtInfo<'_>, property: &str) -> Result<String, OnProbeError> {
    fdt_string(info, property).ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] startup access-point policy requires '{property}'",
            info.node.name()
        ))
    })
}

fn required_u8(info: &FdtInfo<'_>, property: &str) -> Result<u8, OnProbeError> {
    let value = fdt_u32(info, property).ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] startup access-point policy requires '{property}'",
            info.node.name()
        ))
    })?;
    u8::try_from(value).map_err(|_| {
        OnProbeError::other(format!(
            "[{}] property '{property}' does not fit u8",
            info.node.name()
        ))
    })
}

fn required_ipv4(info: &FdtInfo<'_>, property: &str) -> Result<[u8; 4], OnProbeError> {
    fdt_u32(info, property)
        .map(u32::to_be_bytes)
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] startup access-point policy requires '{property}'",
                info.node.name()
            ))
        })
}
