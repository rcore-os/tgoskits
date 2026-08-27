//! CV181x firmware-resource translation shared by SD and SDIO glue.

use alloc::format;
#[cfg(feature = "aic8800-wifi")]
use alloc::string::String;

use cv181x_sdhci::Cv181xConfig;
use fdt_edit::{Phandle, RegFixed};
use log::warn;
use rdrive::{probe::OnProbeError, register::FdtInfo};
use sdmmc_host::BusWidth;

pub(crate) const SDHCI_MIN_MMIO_SIZE: usize = 0x1000;
pub(crate) const SYSCON_MIN_MMIO_SIZE: usize = 0x2000;
#[cfg(feature = "aic8800-wifi")]
pub(crate) const CRG_MIN_MMIO_SIZE: usize = 0x1000;
#[cfg(feature = "aic8800-wifi")]
pub(crate) const RTCSYS_CTRL_MIN_MMIO_SIZE: usize = 0x1000;
#[cfg(feature = "aic8800-wifi")]
pub(crate) const RTCSYS_IO_MIN_MMIO_SIZE: usize = 0x1000;

/// One firmware-described MMIO region after minimum-layout validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MmioRegion {
    pub(crate) address: usize,
    pub(crate) size: usize,
}

impl MmioRegion {
    pub(crate) fn map(self) -> Result<core::ptr::NonNull<u8>, OnProbeError> {
        crate::mmio::iomap(self.address, self.size)
    }
}

/// Resolve the controller register from `reg-names`, falling back to the
/// conventional first `reg` entry used by simple SDHCI bindings.
pub(crate) fn controller_region(
    info: &FdtInfo<'_>,
    name: &str,
    minimum_size: usize,
) -> Result<MmioRegion, OnProbeError> {
    let reg = named_reg(info, name)
        .or_else(|| info.node.regs().into_iter().next())
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] has no controller reg resource",
                info.node.name()
            ))
        })?;
    validate_region(info.node.name(), name, reg, minimum_size)
}

/// Resolve a SoC integration region either as a named entry on the consumer
/// node or through a firmware phandle to a syscon/provider node.
pub(crate) fn required_region(
    info: &FdtInfo<'_>,
    name: &str,
    phandle_property: &str,
    minimum_size: usize,
) -> Result<MmioRegion, OnProbeError> {
    let reg = named_reg(info, name)
        .or_else(|| phandle_reg(info, phandle_property))
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] requires reg-names entry '{name}' or phandle property '{phandle_property}'",
                info.node.name()
            ))
        })?;
    validate_region(info.node.name(), name, reg, minimum_size)
}

/// Translate common SD/MMC firmware properties into portable host policy.
pub(crate) fn host_config(info: &FdtInfo<'_>, prepared_source_hz: Option<u64>) -> Cv181xConfig {
    let defaults = Cv181xConfig::default();
    let source_hz = prepared_source_hz
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| fdt_u32(info, "src-frequency"))
        .unwrap_or(defaults.src_frequency_hz);
    Cv181xConfig {
        src_frequency_hz: source_hz,
        min_frequency_hz: fdt_u32(info, "min-frequency").unwrap_or(defaults.min_frequency_hz),
        max_frequency_hz: fdt_u32(info, "max-frequency").unwrap_or(defaults.max_frequency_hz),
        max_bus_width: bus_width(info),
        no_1v8: has_property(info, "no-1-8-v"),
        has_card_detect_gpio: has_property(info, "cvi-cd-gpios") || has_property(info, "cd-gpios"),
        touch_power_enable_pin: has_property(info, "cvitek,touch-power-enable-pin"),
    }
    .normalized()
}

pub(crate) fn fdt_u32(info: &FdtInfo<'_>, name: &str) -> Option<u32> {
    info.node
        .as_node()
        .get_property(name)
        .and_then(|property| property.get_u32())
}

#[cfg(feature = "aic8800-wifi")]
pub(crate) fn fdt_string(info: &FdtInfo<'_>, name: &str) -> Option<String> {
    info.node
        .as_node()
        .get_property(name)
        .and_then(|property| property.as_str())
        .map(String::from)
}

pub(crate) fn has_property(info: &FdtInfo<'_>, name: &str) -> bool {
    info.node.as_node().get_property(name).is_some()
}

fn named_reg(info: &FdtInfo<'_>, expected: &str) -> Option<RegFixed> {
    let names = info.node.as_node().get_property("reg-names")?.as_str_iter();
    let regs = info.node.regs();
    names.enumerate().find_map(|(index, name)| {
        (name == expected)
            .then(|| regs.get(index).copied())
            .flatten()
    })
}

fn phandle_reg(info: &FdtInfo<'_>, property: &str) -> Option<RegFixed> {
    let phandle = info
        .node
        .as_node()
        .get_property(property)?
        .get_u32()
        .map(Phandle::from)?;
    info.get_by_phandle(phandle)?.regs().into_iter().next()
}

fn validate_region(
    node_name: &str,
    resource_name: &str,
    reg: RegFixed,
    minimum_size: usize,
) -> Result<MmioRegion, OnProbeError> {
    let address = usize::try_from(reg.address).map_err(|_| {
        OnProbeError::other(format!(
            "[{node_name}] resource '{resource_name}' address does not fit usize"
        ))
    })?;
    let declared_size = reg.size.unwrap_or(minimum_size as u64);
    if declared_size < minimum_size as u64 {
        return Err(OnProbeError::other(format!(
            "[{node_name}] resource '{resource_name}' size 0x{declared_size:x} is smaller than \
             required 0x{minimum_size:x}"
        )));
    }
    let size = usize::try_from(declared_size).map_err(|_| {
        OnProbeError::other(format!(
            "[{node_name}] resource '{resource_name}' size does not fit usize"
        ))
    })?;
    Ok(MmioRegion { address, size })
}

fn bus_width(info: &FdtInfo<'_>) -> BusWidth {
    match fdt_u32(info, "bus-width").unwrap_or(4) {
        1 => BusWidth::Bit1,
        4 => BusWidth::Bit4,
        8 => {
            warn!(
                "[{}] 8-bit bus-width requested for CV181x 4-bit pads; clamping to 4-bit",
                info.node.name()
            );
            BusWidth::Bit4
        }
        other => {
            warn!(
                "[{}] unsupported bus-width {other}; using 4-bit",
                info.node.name()
            );
            BusWidth::Bit4
        }
    }
}

#[cfg(test)]
mod tests {
    use fdt_edit::Fdt;

    const AKA_00_DTB: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../os/StarryOS/configs/board/aka-00-sg2002.dtb"
    ));
    const LICHEERV_NANO_DTB: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../os/StarryOS/configs/board/licheerv-nano-sg2002.dtb"
    ));

    #[test]
    fn repository_sg2002_dtbs_describe_every_cv181x_sd_and_sdio_region() {
        for (board, bytes) in [
            ("aka-00-sg2002", AKA_00_DTB),
            ("licheerv-nano-sg2002", LICHEERV_NANO_DTB),
        ] {
            let fdt = Fdt::from_bytes(bytes).expect("repository board DTB must parse");
            assert_named_regions(
                board,
                &fdt,
                "/cv-sd@4310000",
                &[
                    ("sdio", 0x0431_0000, 0x1000),
                    ("syscon", 0x0300_0000, 0x8000),
                ],
            );
            assert_named_regions(
                board,
                &fdt,
                "/wifi-sd@4320000",
                &[
                    ("sdio", 0x0432_0000, 0x1000),
                    ("syscon", 0x0300_0000, 0x8000),
                    ("crg", 0x0300_2000, 0x1000),
                    ("rtcsys-ctrl", 0x0502_5000, 0x1000),
                    ("rtcsys-io", 0x0502_7000, 0x1000),
                ],
            );
        }
    }

    fn assert_named_regions(board: &str, fdt: &Fdt, path: &str, expected: &[(&str, u64, u64)]) {
        let node = fdt
            .get_by_path(path)
            .unwrap_or_else(|| panic!("{board}: missing {path}"));
        let names = node
            .as_node()
            .get_property("reg-names")
            .unwrap_or_else(|| panic!("{board}: {path} has no reg-names"))
            .as_str_iter()
            .collect::<alloc::vec::Vec<_>>();
        let regs = node.regs();

        assert_eq!(names.len(), regs.len(), "{board}: {path} reg/name count");
        assert_eq!(names.len(), expected.len(), "{board}: {path} region count");
        for ((name, reg), (expected_name, expected_address, expected_size)) in
            names.into_iter().zip(regs).zip(expected)
        {
            assert_eq!(name, *expected_name, "{board}: {path} region name");
            assert_eq!(
                reg.address, *expected_address,
                "{board}: {path}/{name} address"
            );
            assert_eq!(
                reg.size,
                Some(*expected_size),
                "{board}: {path}/{name} size"
            );
        }
    }
}
