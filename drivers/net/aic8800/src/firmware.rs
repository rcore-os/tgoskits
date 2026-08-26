//! Build-provisioned firmware images and startup constants.

use crate::common::ChipVariant;

macro_rules! firmware_path {
    ($name:literal) => {
        concat!(env!("OUT_DIR"), "/firmware/", $name)
    };
}

static AIC8801_MAIN: &[u8] = include_bytes!(firmware_path!("fmacfw.bin"));
static AIC8801_PATCH: &[u8] = include_bytes!(firmware_path!("fmacfw_patch.bin"));
static AIC8800D80_MAIN: &[u8] = include_bytes!(firmware_path!("fmacfw_8800d80_u02.bin"));

pub(crate) const MAIN_ADDRESS: u32 = 0x0012_0000;
pub(crate) const PATCH_ADDRESS: u32 = 0x0019_0000;
pub(crate) const UPLOAD_CHUNK: usize = 1024;
pub(crate) const CONFIG_BASE_OFFSET: u32 = 0x0180;
pub(crate) const PATCH_ADDRESS_REGISTER: u32 = 0x001e_5318;
pub(crate) const PATCH_COUNT_REGISTER: u32 = 0x001e_531c;
pub(crate) const PATCH_TABLE_ADDRESS: u32 = 0x001e_6000;
pub(crate) const PATCH_TABLE: &[[u32; 2]] = &[[0x0104, 0]];
pub(crate) const SYSTEM_CONFIG: &[(u32, u32)] = &[
    (0x4050_0014, 0x0000_0101),
    (0x4050_0018, 0x0000_0109),
    (0x4050_0004, 0x0000_0010),
    (0x4004_0000, 0x0000_1ac8),
    (0x4004_0084, 0x0001_1580),
    (0x4004_0080, 0x0000_0001),
    (0x4010_0058, 0),
    (0x5000_0000, 0x0322_0204),
    (0x5001_9150, 0x0000_0002),
    (0x5001_7008, 0),
];
pub(crate) const MASKED_SYSTEM_CONFIG: &[(u32, u32, u32)] = &[
    (0x4050_6024, 0x0000_00ff, 0x0000_00df),
    (0x4034_4058, 0x0080_0000, 0),
];

pub(crate) fn images(chip: ChipVariant) -> Option<(&'static [u8], &'static [u8])> {
    match chip {
        ChipVariant::Aic8801 => Some((AIC8801_MAIN, AIC8801_PATCH)),
        ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2 => Some((AIC8800D80_MAIN, &[])),
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW | ChipVariant::Unknown => None,
    }
}
