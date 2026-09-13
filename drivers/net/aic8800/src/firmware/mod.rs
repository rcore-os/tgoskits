//! Build-provisioned firmware images and chip-specific startup data.

mod dc_config;
mod dc_lmac_rf;
mod dc_rf;

pub(crate) use dc_config::*;
pub(crate) use dc_lmac_rf::*;
pub(crate) use dc_rf::*;

macro_rules! firmware_path {
    ($name:literal) => {
        concat!(env!("OUT_DIR"), "/firmware/", $name)
    };
}

static AIC8800D80_MAIN: &[u8] = include_bytes!(firmware_path!("fmacfw_8800d80_u02.bin"));
static AIC8800DC_U02_PATCH: &[u8] = include_bytes!(firmware_path!("fmacfw_patch_8800dc_u02.bin"));
static AIC8800DC_U02_PATCH_TABLE: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_tbl_8800dc_u02.bin"));
static AIC8800DC_H_U02_PATCH: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_8800dc_h_u02.bin"));
static AIC8800DC_H_U02_PATCH_TABLE: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_tbl_8800dc_h_u02.bin"));
static AIC8800DC_U02_CALIBRATION: &[u8] =
    include_bytes!(firmware_path!("fmacfw_calib_8800dc_u02.bin"));
static AIC8800DC_H_U02_CALIBRATION: &[u8] =
    include_bytes!(firmware_path!("fmacfw_calib_8800dc_h_u02.bin"));
static AIC8800DC_HBT_U02_PATCH: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_8800dc_hbt_u02.bin"));
static AIC8800DC_HBT_U02_PATCH_TABLE: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_tbl_8800dc_hbt_u02.bin"));
static AIC8800DC_HBT_U02_CALIBRATION: &[u8] =
    include_bytes!(firmware_path!("fmacfw_calib_8800dc_hbt_u02.bin"));

pub(crate) const D80_MAIN_ADDRESS: u32 = 0x0012_0000;

pub(crate) const FIRMWARE_UPLOAD_CHUNK: usize = 1024;
pub(crate) const DC_CONFIG_UPLOAD_CHUNK: usize = 512;
pub(crate) const DC_PATCH_ADDRESS: u32 = 0x0018_0000;
pub(crate) const DC_CALIBRATION_ADDRESS: u32 = 0x0013_0000;
pub(crate) const DC_CALIBRATION_ENTRY: u32 = DC_CALIBRATION_ADDRESS + 9;
pub(crate) const DC_CONFIG_BASE: u32 = 0x0001_0164;
pub(crate) const DC_BOOT_ADDRESS: u32 = 0x0012_0000;
pub(crate) const DC_PATCH_DESCRIPTION_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DcRevision {
    U02,
    HighPerformanceU02,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DcFirmwareVariant {
    U02,
    HighPerformanceU02,
    HighPerformanceBluetoothU02,
}

pub(crate) struct DcFirmwareImages {
    pub(crate) patch: &'static [u8],
    pub(crate) patch_table: &'static [u8],
    pub(crate) calibration: &'static [u8],
}

pub(crate) const fn d80_main_image() -> &'static [u8] {
    AIC8800D80_MAIN
}

pub(crate) const fn dc_images(variant: DcFirmwareVariant) -> DcFirmwareImages {
    match variant {
        DcFirmwareVariant::U02 => DcFirmwareImages {
            patch: AIC8800DC_U02_PATCH,
            patch_table: AIC8800DC_U02_PATCH_TABLE,
            calibration: AIC8800DC_U02_CALIBRATION,
        },
        DcFirmwareVariant::HighPerformanceU02 => DcFirmwareImages {
            patch: AIC8800DC_H_U02_PATCH,
            patch_table: AIC8800DC_H_U02_PATCH_TABLE,
            calibration: AIC8800DC_H_U02_CALIBRATION,
        },
        DcFirmwareVariant::HighPerformanceBluetoothU02 => DcFirmwareImages {
            patch: AIC8800DC_HBT_U02_PATCH,
            patch_table: AIC8800DC_HBT_U02_PATCH_TABLE,
            calibration: AIC8800DC_HBT_U02_CALIBRATION,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_supported_dc_revision_has_complete_wifi_assets() {
        for variant in [
            DcFirmwareVariant::U02,
            DcFirmwareVariant::HighPerformanceU02,
            DcFirmwareVariant::HighPerformanceBluetoothU02,
        ] {
            let images = dc_images(variant);
            assert!(!images.patch.is_empty());
            assert!(images.patch_table.len() >= DC_PATCH_DESCRIPTION_BYTES);
            assert!(!images.calibration.is_empty());
        }
    }
}
