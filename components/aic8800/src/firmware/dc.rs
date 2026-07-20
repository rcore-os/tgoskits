//! AIC8800DC/DW firmware sequencing.
//!
//! The ordering follows the hardware protocol used by the vendor BSP, but is
//! represented as a request/confirmation machine so an OS can drive it from a
//! CPU-pinned maintenance task without sleeping or polling inside the driver.

use super::{
    AIC8800_DC, AIC8800_DC_H, AIC8800_DC_H_CALIB, AIC8800_DC_H_PATCH_TABLE, AIC8800_DC_PATCH_TABLE,
    DebugCompletion, DebugOperation, FirmwareError, RAM_FMAC_FW_ADDR, ROM_FMAC_CALIB_ADDR,
    ROM_FMAC_PATCH_ADDR, dc_rf_cfg,
};
use crate::common::{HOST_START_APP_DUMMY, HOST_START_APP_FNCALL};

const CHIP_ID_ADDR: u32 = 0x4050_0000;
const CHIP_SUB_ID_ADDR: u32 = 0x0000_0020;
const CRYSTAL_STATUS_ADDR: u32 = 0x4050_0148;
const BBPLL_CONFIG_ADDR: u32 = 0x4050_5010;
const CFG_BASE: u32 = 0x0001_0164;
const CHIP_ID_H_MASK: u8 = 0xc0;
const CFG_CHUNK_BYTES: usize = 512;
const PATCH_DESCRIPTION_BYTES: usize = 128;

const SYSCFG: &[(u32, u32)] = &[(0x4050_0010, 4), (0x4050_0010, 6)];

const SDIO_U02_SYSCFG: &[(u32, u32)] = &[
    (0x4003_0000, 0x0003_6da4),
    (0x0011_e800, 0xe7fe_4070),
    (0x4003_0084, 0x0011_e800),
    (0x4003_0080, 1),
    (0x4010_001c, 0),
];

const MASKED_SYSCFG: &[(u32, u32, u32)] = &[
    (0x7000_216c, 3 << 2, 1 << 2),
    (0x7000_21bc, 3 << 2, 1 << 2),
    (0x7000_2118, (7 << 4) | (1 << 7), (2 << 4) | (1 << 7)),
    (0x7000_2104, 0x3f | (1 << 6), 2 | (1 << 6)),
    (0x7000_210c, 0x3f | (1 << 6), 2 | (1 << 6)),
    (0x7000_2170, 0xf, 1),
    (0x7000_2190, 0x3f, 24),
    (0x7000_21cc, (7 << 4) | (1 << 7), 0),
    (0x7000_10a0, 1 << 11, 1 << 11),
    (0x7000_1034, (1 << 20) | (7 << 26), 2 << 26),
    (0x7000_1038, 1 << 8, 1 << 8),
    (0x7000_1094, 3 << 2, 0),
    (0x7000_21d0, (1 << 5) | (1 << 6), (1 << 5) | (1 << 6)),
    (
        0x7000_1000,
        (1 << 0) | (1 << 20) | (1 << 22),
        (1 << 0) | (1 << 20),
    ),
    (0x7000_1028, 0xf << 2, 1 << 2),
];

const H_MASKED_SYSCFG: &[(u32, u32, u32)] = &[
    (0x7000_216c, (3 << 2) | (3 << 4), (2 << 2) | (2 << 4)),
    (0x7000_2138, 0xff, 0xff),
    (0x7000_213c, 0xff, 0xff),
    (0x7000_2144, 0xff, 0xff),
    (0x7000_21bc, 3 << 2, 1 << 2),
    (0x7000_2118, (7 << 4) | (1 << 7), (2 << 4) | (1 << 7)),
    (0x7000_2104, 0x3f | (1 << 6), 2 | (1 << 6)),
    (0x7000_210c, 0x3f | (1 << 6), 2 | (1 << 6)),
    (0x7000_2170, 0xf, 1),
    (0x7000_2190, 0x3f, 24),
    (0x7000_21cc, (7 << 4) | (1 << 7), 0),
    (0x7000_10a0, 1 << 11, 1 << 11),
    (0x7000_1038, 1 << 8, 1 << 8),
    (0x7000_1094, 3 << 2, 0),
    (0x7000_21d0, (1 << 5) | (1 << 6), (1 << 5) | (1 << 6)),
    (
        0x7000_1000,
        (1 << 0) | (1 << 20) | (1 << 22),
        (1 << 0) | (1 << 20),
    ),
    (0x7000_1028, 0xf << 2, 1 << 2),
];

const U01_MASKED_SYSCFG: &[(u32, u32, u32)] = &[
    (0x7000_1000, 1 << 16, 1 << 16),
    (0x7000_1028, 1 << 6, 1 << 6),
    (0x7000_1000, 1 << 16, 0),
];

#[derive(Clone, Copy)]
enum Step {
    ReadChipId,
    ReadSubId,
    ReadCrystalStatus,
    ReadBbpll,
    WriteBbpll(u32),
    ReadSystemClock,
    Syscfg(usize),
    SdioSyscfg(usize),
    MaskedSyscfg(usize),
    U01MaskedSyscfg(usize),
    UploadPatch(usize),
    ReadMiscAddress,
    ReadMiscMask(usize),
    ZeroMisc(usize),
    UploadCalibration(usize),
    StartCalibration,
    ReadWifiConfig,
    ReadLdpcConfig,
    ReadAgcConfig,
    ReadTxgainConfig,
    ReadWifiSetting,
    WriteWifiSetting(u32),
    UploadLdpc(usize),
    UploadAgc(usize),
    UploadTxgain(usize),
    PatchDescription,
    PatchPairs(usize),
    ReadEntry,
    StartFirmware,
    Waiting,
    Done,
}

#[derive(Clone, Copy)]
enum ReadEffect {
    ChipId,
    SubId,
    CrystalStatus,
    Bbpll,
    SystemClock,
    MiscAddress,
    MiscMask(usize),
    WifiConfig,
    LdpcConfig,
    AgcConfig,
    TxgainConfig,
    WifiSetting,
    Entry,
}

#[derive(Clone, Copy)]
enum CompletionEffect {
    Read(ReadEffect),
    Complete,
}

pub(super) struct DcFirmware {
    step: Step,
    effect: Option<CompletionEffect>,
    chip_id: u8,
    sub_id: u8,
    mcu_id: u8,
    misc_address: u32,
    misc_mask: [u32; 4],
    wifi_config: u32,
    ldpc_config: u32,
    agc_config: u32,
    txgain_config: u32,
}

impl DcFirmware {
    pub(super) const fn new() -> Self {
        Self {
            step: Step::ReadChipId,
            effect: None,
            chip_id: 0,
            sub_id: 0,
            mcu_id: 0,
            misc_address: 0,
            misc_mask: [0; 4],
            wifi_config: 0,
            ldpc_config: 0,
            agc_config: 0,
            txgain_config: 0,
        }
    }

    fn is_h(&self) -> bool {
        self.chip_id & CHIP_ID_H_MASK == CHIP_ID_H_MASK
    }

    pub(super) fn next_operation(&mut self) -> Result<Option<DebugOperation>, FirmwareError> {
        debug_assert!(self.effect.is_none());
        loop {
            let step = self.step;
            match step {
                Step::ReadChipId => return Ok(Some(self.read(CHIP_ID_ADDR, ReadEffect::ChipId))),
                Step::ReadSubId => {
                    return Ok(Some(self.read(CHIP_SUB_ID_ADDR, ReadEffect::SubId)));
                }
                Step::ReadCrystalStatus => {
                    return Ok(Some(
                        self.read(CRYSTAL_STATUS_ADDR, ReadEffect::CrystalStatus),
                    ));
                }
                Step::ReadBbpll => {
                    return Ok(Some(self.read(BBPLL_CONFIG_ADDR, ReadEffect::Bbpll)));
                }
                Step::WriteBbpll(value) => {
                    return Ok(Some(self.issue_complete(
                        DebugOperation::Write {
                            address: BBPLL_CONFIG_ADDR,
                            value,
                        },
                        Step::ReadSystemClock,
                    )));
                }
                Step::ReadSystemClock => {
                    return Ok(Some(self.read(0x4050_0010, ReadEffect::SystemClock)));
                }
                Step::Syscfg(index) => {
                    if let Some(&(address, value)) = SYSCFG.get(index) {
                        return Ok(Some(self.issue_complete(
                            DebugOperation::Write { address, value },
                            Step::Syscfg(index + 1),
                        )));
                    }
                    self.step = Step::SdioSyscfg(0);
                }
                Step::SdioSyscfg(index) => {
                    if self.mcu_id == 0
                        && matches!(self.sub_id, 1 | 2)
                        && let Some(&(address, value)) = SDIO_U02_SYSCFG.get(index)
                    {
                        return Ok(Some(self.issue_complete(
                            DebugOperation::Write { address, value },
                            Step::SdioSyscfg(index + 1),
                        )));
                    }
                    self.step = Step::MaskedSyscfg(0);
                }
                Step::MaskedSyscfg(index) => {
                    let table = if self.is_h() {
                        H_MASKED_SYSCFG
                    } else {
                        MASKED_SYSCFG
                    };
                    if let Some(&(address, mut mask, mut value)) = table.get(index) {
                        if address == 0x7000_1000 && self.mcu_id == 0 {
                            let extra = (1 << 8) | (1 << 15);
                            mask |= extra;
                            value |= extra;
                        }
                        return Ok(Some(self.issue_complete(
                            DebugOperation::MaskWrite {
                                address,
                                mask,
                                value,
                            },
                            Step::MaskedSyscfg(index + 1),
                        )));
                    }
                    self.step = Step::U01MaskedSyscfg(0);
                }
                Step::U01MaskedSyscfg(index) => {
                    if self.sub_id == 0 {
                        if let Some(&(address, mask, value)) = U01_MASKED_SYSCFG.get(index) {
                            return Ok(Some(self.issue_complete(
                                DebugOperation::MaskWrite {
                                    address,
                                    mask,
                                    value,
                                },
                                Step::U01MaskedSyscfg(index + 1),
                            )));
                        }
                        return Err(FirmwareError::UnsupportedDcRevision {
                            chip_id: self.chip_id,
                            sub_id: self.sub_id,
                        });
                    }
                    self.step = Step::UploadPatch(0);
                }
                Step::UploadPatch(offset) => {
                    let image = if self.is_h() {
                        AIC8800_DC_H
                    } else {
                        AIC8800_DC
                    };
                    return Ok(Some(self.upload(
                        image,
                        ROM_FMAC_PATCH_ADDR,
                        offset,
                        Step::ReadMiscAddress,
                    )));
                }
                Step::ReadMiscAddress => {
                    return Ok(Some(self.read(CFG_BASE + 0x14, ReadEffect::MiscAddress)));
                }
                Step::ReadMiscMask(index) => {
                    return Ok(Some(self.read(
                        self.misc_address.wrapping_add(index as u32 * 4),
                        ReadEffect::MiscMask(index),
                    )));
                }
                Step::ZeroMisc(index) => {
                    if index < 3 {
                        return Ok(Some(self.issue_complete(
                            DebugOperation::Write {
                                address: self.misc_address.wrapping_add(index as u32 * 4),
                                value: 0,
                            },
                            Step::ZeroMisc(index + 1),
                        )));
                    }
                    self.step = Step::ReadWifiConfig;
                }
                Step::UploadCalibration(offset) => {
                    return Ok(Some(self.upload(
                        AIC8800_DC_H_CALIB,
                        ROM_FMAC_CALIB_ADDR,
                        offset,
                        Step::StartCalibration,
                    )));
                }
                Step::StartCalibration => {
                    return Ok(Some(self.issue_complete(
                        DebugOperation::StartApp {
                            address: ROM_FMAC_CALIB_ADDR + 9,
                            boot_type: HOST_START_APP_FNCALL,
                        },
                        Step::ReadWifiConfig,
                    )));
                }
                Step::ReadWifiConfig => {
                    return Ok(Some(self.read(CFG_BASE, ReadEffect::WifiConfig)));
                }
                Step::ReadLdpcConfig => {
                    return Ok(Some(self.read(CFG_BASE + 8, ReadEffect::LdpcConfig)));
                }
                Step::ReadAgcConfig => {
                    return Ok(Some(self.read(CFG_BASE + 0x0c, ReadEffect::AgcConfig)));
                }
                Step::ReadTxgainConfig => {
                    return Ok(Some(self.read(CFG_BASE + 0x10, ReadEffect::TxgainConfig)));
                }
                Step::ReadWifiSetting => {
                    return Ok(Some(self.read(
                        self.wifi_config.wrapping_add(0x124),
                        ReadEffect::WifiSetting,
                    )));
                }
                Step::WriteWifiSetting(value) => {
                    return Ok(Some(self.issue_complete(
                        DebugOperation::Write {
                            address: self.wifi_config.wrapping_add(0x124),
                            value,
                        },
                        Step::UploadLdpc(0),
                    )));
                }
                Step::UploadLdpc(offset) => {
                    return Ok(Some(self.upload_cfg(
                        dc_rf_cfg::FW_DC_LDPC_CFG,
                        self.ldpc_config,
                        offset,
                        Step::UploadAgc(0),
                    )));
                }
                Step::UploadAgc(offset) => {
                    return Ok(Some(self.upload_cfg(
                        dc_rf_cfg::FW_DC_AGC_CFG,
                        self.agc_config,
                        offset,
                        Step::UploadTxgain(0),
                    )));
                }
                Step::UploadTxgain(offset) => {
                    let bytes = if self.is_h() {
                        dc_rf_cfg::FW_DC_TXGAIN_MAP_H
                    } else {
                        dc_rf_cfg::FW_DC_TXGAIN_MAP
                    };
                    return Ok(Some(self.upload_cfg(
                        bytes,
                        self.txgain_config,
                        offset,
                        Step::PatchDescription,
                    )));
                }
                Step::PatchDescription => {
                    let table = self.patch_table()?;
                    let address = read_table_u32(table, 0)?;
                    return Ok(Some(self.issue_complete(
                        DebugOperation::block_write(address, &table[..PATCH_DESCRIPTION_BYTES]),
                        Step::PatchPairs(PATCH_DESCRIPTION_BYTES),
                    )));
                }
                Step::PatchPairs(offset) => {
                    let table = self.patch_table()?;
                    if offset == table.len() {
                        self.step = Step::ReadEntry;
                        continue;
                    }
                    let address = read_table_u32(table, offset)?;
                    let value = read_table_u32(table, offset + 4)?;
                    return Ok(Some(self.issue_complete(
                        DebugOperation::Write { address, value },
                        Step::PatchPairs(offset + 8),
                    )));
                }
                Step::ReadEntry => {
                    return Ok(Some(self.read(RAM_FMAC_FW_ADDR, ReadEffect::Entry)));
                }
                Step::StartFirmware => {
                    return Ok(Some(self.issue_complete(
                        DebugOperation::StartApp {
                            address: RAM_FMAC_FW_ADDR,
                            boot_type: HOST_START_APP_DUMMY,
                        },
                        Step::Done,
                    )));
                }
                Step::Waiting => return Err(FirmwareError::UnexpectedConfirmation),
                Step::Done => return Ok(None),
            }
        }
    }

    pub(super) fn complete(&mut self, completion: DebugCompletion) -> Result<(), FirmwareError> {
        let effect = self
            .effect
            .take()
            .ok_or(FirmwareError::UnexpectedConfirmation)?;
        match (effect, completion) {
            (CompletionEffect::Complete, DebugCompletion::Complete) => Ok(()),
            (CompletionEffect::Read(effect), DebugCompletion::Read { value }) => {
                self.complete_read(effect, value)
            }
            _ => Err(FirmwareError::UnexpectedConfirmation),
        }
    }

    fn complete_read(&mut self, effect: ReadEffect, value: u32) -> Result<(), FirmwareError> {
        self.step = match effect {
            ReadEffect::ChipId => {
                self.chip_id = (value >> 16) as u8;
                self.mcu_id = if value & (1 << 25) == 0 { 1 } else { 0 };
                Step::ReadSubId
            }
            ReadEffect::SubId => {
                self.sub_id = value as u8;
                Step::ReadCrystalStatus
            }
            ReadEffect::CrystalStatus if value & 1 == 0 => Step::ReadSystemClock,
            ReadEffect::CrystalStatus => Step::ReadBbpll,
            ReadEffect::Bbpll if value >> 29 == 3 => Step::ReadSystemClock,
            ReadEffect::Bbpll => Step::WriteBbpll((value | (1 << 29) | (1 << 30)) & !(1 << 31)),
            ReadEffect::SystemClock => Step::Syscfg(0),
            ReadEffect::MiscAddress => {
                self.misc_address = value;
                if self.is_h() {
                    Step::ReadMiscMask(0)
                } else {
                    Step::ZeroMisc(0)
                }
            }
            ReadEffect::MiscMask(index) => {
                self.misc_mask[index] = value;
                if index + 1 < self.misc_mask.len() {
                    Step::ReadMiscMask(index + 1)
                } else if self.misc_ram_valid() {
                    Step::ReadWifiConfig
                } else {
                    Step::UploadCalibration(0)
                }
            }
            ReadEffect::WifiConfig => {
                self.wifi_config = value;
                Step::ReadLdpcConfig
            }
            ReadEffect::LdpcConfig => {
                self.ldpc_config = value;
                Step::ReadAgcConfig
            }
            ReadEffect::AgcConfig => {
                self.agc_config = value;
                Step::ReadTxgainConfig
            }
            ReadEffect::TxgainConfig => {
                self.txgain_config = value;
                Step::ReadWifiSetting
            }
            ReadEffect::WifiSetting => Step::WriteWifiSetting((value & 0xff00_0000) | 0x0000_1e01),
            ReadEffect::Entry => Step::StartFirmware,
        };
        Ok(())
    }

    fn read(&mut self, address: u32, effect: ReadEffect) -> DebugOperation {
        self.effect = Some(CompletionEffect::Read(effect));
        self.step = Step::Waiting;
        DebugOperation::Read { address }
    }

    fn issue_complete(&mut self, operation: DebugOperation, next: Step) -> DebugOperation {
        self.effect = Some(CompletionEffect::Complete);
        self.step = next;
        operation
    }

    fn upload(
        &mut self,
        bytes: &'static [u8],
        address: u32,
        offset: usize,
        done: Step,
    ) -> DebugOperation {
        self.upload_with_chunk(bytes, address, offset, done, super::UPLOAD_CHUNK_BYTES)
    }

    fn upload_cfg(
        &mut self,
        bytes: &'static [u8],
        address: u32,
        offset: usize,
        done: Step,
    ) -> DebugOperation {
        self.upload_with_chunk(bytes, address, offset, done, CFG_CHUNK_BYTES)
    }

    fn upload_with_chunk(
        &mut self,
        bytes: &'static [u8],
        address: u32,
        offset: usize,
        done: Step,
        chunk_bytes: usize,
    ) -> DebugOperation {
        let end = (offset + chunk_bytes).min(bytes.len());
        let next = if end == bytes.len() {
            done
        } else {
            match self.step {
                Step::UploadPatch(_) => Step::UploadPatch(end),
                Step::UploadCalibration(_) => Step::UploadCalibration(end),
                Step::UploadLdpc(_) => Step::UploadLdpc(end),
                Step::UploadAgc(_) => Step::UploadAgc(end),
                Step::UploadTxgain(_) => Step::UploadTxgain(end),
                _ => unreachable!("only upload states use upload_with_chunk"),
            }
        };
        self.issue_complete(
            DebugOperation::block_write(address.wrapping_add(offset as u32), &bytes[offset..end]),
            next,
        )
    }

    fn misc_ram_valid(&self) -> bool {
        self.misc_mask[0] == 0
            && self.misc_mask[1] & 0xfff0_0000 == 0x8000_0000
            && self.misc_mask[2] == 0
            && self.misc_mask[3] & 0xffff_ff00 == 0
    }

    fn patch_table(&self) -> Result<&'static [u8], FirmwareError> {
        let table = if self.is_h() {
            AIC8800_DC_H_PATCH_TABLE
        } else {
            AIC8800_DC_PATCH_TABLE
        };
        if table.len() < PATCH_DESCRIPTION_BYTES {
            return Err(FirmwareError::InvalidPatchTable {
                reason: "description is truncated",
            });
        }
        if !(table.len() - PATCH_DESCRIPTION_BYTES).is_multiple_of(8) {
            return Err(FirmwareError::InvalidPatchTable {
                reason: "address/value tail is not pair-aligned",
            });
        }
        Ok(table)
    }
}

fn read_table_u32(table: &[u8], offset: usize) -> Result<u32, FirmwareError> {
    let bytes = table
        .get(offset..offset + 4)
        .ok_or(FirmwareError::InvalidPatchTable {
            reason: "address/value pair is truncated",
        })?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}
