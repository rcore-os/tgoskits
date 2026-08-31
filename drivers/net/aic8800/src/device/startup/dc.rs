//! AIC8800DC U02/H-U02 firmware startup state machine.

use alloc::vec::Vec;

use super::{StartupStage, map_debug_error};
use crate::{
    common::HOST_START_APP_FNCALL,
    device::*,
    firmware::{
        DC_AGC_CONFIG, DC_BOOT_ADDRESS, DC_CALIBRATION_ADDRESS, DC_CALIBRATION_ENTRY,
        DC_CONFIG_BASE, DC_CONFIG_UPLOAD_CHUNK, DC_LDPC_CONFIG, DC_MASKED_SYSTEM_CONFIG,
        DC_MASKED_SYSTEM_CONFIG_H, DC_MCU_SYSTEM_CONFIG_U02, DC_PATCH_ADDRESS,
        DC_PATCH_DESCRIPTION_BYTES, DC_RX_GAIN_TABLE_20M, DC_RX_GAIN_TABLE_40M, DC_SYSTEM_CONFIG,
        DC_TX_GAIN_CONFIG, DC_TX_GAIN_CONFIG_H, DC_TX_GAIN_TABLE_0, DC_TX_GAIN_TABLE_1,
        DC_TX_GAIN_TABLE_H_0, DC_TX_GAIN_TABLE_H_1, DC_WIFI_SETTING_OFFSET_U02,
        DC_WIFI_SETTING_U02, DcFirmwareVariant, DcRevision, FIRMWARE_UPLOAD_CHUNK, dc_images,
    },
    lmac::{RfTableSelection, rf_config_payload},
    protocol::{
        DBG_MEM_BLOCK_WRITE_REQ, DBG_MEM_MASK_WRITE_REQ, DBG_MEM_READ_REQ, DBG_MEM_WRITE_REQ,
        DBG_START_APP_REQ, debug_memory_read, memory_block_write_payload,
        memory_mask_write_payload, memory_read_payload, memory_write_payload,
        require_debug_memory_write, require_debug_status, start_app_payload,
    },
};

const DC_SUB_ID_ADDRESS: u32 = 0x0000_0020;
const DC_CRYSTAL_STATUS_ADDRESS: u32 = 0x4050_0148;
const DC_BBPLL_CONFIG_ADDRESS: u32 = 0x4050_5010;
const DC_CONFIG_POINTER_OFFSETS: [u32; 4] = [0, 8, 12, 16];
const DPD_BIT_MASK_WORDS: usize = 4;
const DPD_HIGH_WORDS: usize = 96;
const DPD_LOFT_WORDS: usize = 18;
const DPD_HIGH_OFFSET: u32 = 16;
const DPD_LOFT_OFFSET: u32 = 0x06d0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DpdSegment {
    BitMask,
    High,
    Loft,
}

impl DpdSegment {
    const fn offset(self) -> u32 {
        match self {
            Self::BitMask => 0,
            Self::High => DPD_HIGH_OFFSET,
            Self::Loft => DPD_LOFT_OFFSET,
        }
    }

    const fn word_count(self) -> usize {
        match self {
            Self::BitMask => DPD_BIT_MASK_WORDS,
            Self::High => DPD_HIGH_WORDS,
            Self::Loft => DPD_LOFT_WORDS,
        }
    }

    const fn next(self) -> Option<Self> {
        match self {
            Self::BitMask => Some(Self::High),
            Self::High => Some(Self::Loft),
            Self::Loft => None,
        }
    }
}

struct DpdCalibrationResult {
    bit_mask: [u32; DPD_BIT_MASK_WORDS],
    high: [u32; DPD_HIGH_WORDS],
    loft: [u32; DPD_LOFT_WORDS],
}

impl DpdCalibrationResult {
    const fn new() -> Self {
        Self {
            bit_mask: [0; DPD_BIT_MASK_WORDS],
            high: [0; DPD_HIGH_WORDS],
            loft: [0; DPD_LOFT_WORDS],
        }
    }

    fn install_word(
        &mut self,
        segment: DpdSegment,
        index: usize,
        value: u32,
    ) -> Result<(), AicError> {
        let destination = match segment {
            DpdSegment::BitMask => self.bit_mask.get_mut(index),
            DpdSegment::High => self.high.get_mut(index),
            DpdSegment::Loft => self.loft.get_mut(index),
        }
        .ok_or(AicError::CompletionMismatch)?;
        *destination = value;
        Ok(())
    }

    fn segment_bytes(&self, segment: DpdSegment) -> Vec<u8> {
        let words: &[u32] = match segment {
            DpdSegment::BitMask => &self.bit_mask,
            DpdSegment::High => &self.high,
            DpdSegment::Loft => &self.loft,
        };
        let mut bytes = Vec::with_capacity(words.len() * 4);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    const fn should_apply(&self) -> bool {
        self.bit_mask[1] != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DcStage {
    ReadSubId,
    ReadCrystalStatus,
    ReadBbpll,
    WriteBbpll,
    SystemConfig(usize),
    UploadPatch(usize),
    ReadMiscRamAddress,
    ReadMiscRamWord(usize),
    UploadCalibration(usize),
    RunCalibration,
    ReadDpdMiscRamAddress,
    ReadDpdResult { segment: DpdSegment, index: usize },
    ReadConfigPointer(usize),
    WriteWifiSetting,
    UploadLdpc(usize),
    UploadAgc(usize),
    UploadTxGain(usize),
    WritePatchDescription,
    WritePatchEntry(usize),
    ReadBootAddress,
    ReadRuntimeMiscRamAddress,
    ApplyDpd(DpdSegment),
}

pub(super) struct DcStartupState {
    chip_id: u8,
    mcu_id: u8,
    bluetooth_enabled: bool,
    revision: Option<DcRevision>,
    bbpll_value: Option<u32>,
    misc_ram_address: Option<u32>,
    misc_ram_words: [u32; 4],
    dpd_result: Option<DpdCalibrationResult>,
    config_addresses: [u32; 4],
}

impl DcStartupState {
    pub(super) fn from_chip_word(raw: u32) -> Result<Self, AicError> {
        let revision = ((raw >> 16) & 0x3f) as u8;
        if !matches!(revision, 1 | 3 | 7) {
            return Err(AicError::UnsupportedRevision(revision));
        }
        Ok(Self {
            chip_id: (raw >> 16) as u8,
            mcu_id: u8::from((raw >> 25) & 1 == 0),
            bluetooth_enabled: (raw >> 26) & 1 != 0,
            revision: None,
            bbpll_value: None,
            misc_ram_address: None,
            misc_ram_words: [0; 4],
            dpd_result: None,
            config_addresses: [0; 4],
        })
    }

    fn install_sub_id(&mut self, sub_id: u8) -> Result<(), AicError> {
        let is_h = self.chip_id & 0xc0 == 0xc0;
        self.revision = Some(match (sub_id, is_h) {
            (1, false) => DcRevision::U02,
            (2, true) => DcRevision::HighPerformanceU02,
            (0, _) => return Err(AicError::UnsupportedRevision(0)),
            (other, _) => return Err(AicError::UnsupportedRevision(other)),
        });
        Ok(())
    }

    fn revision(&self) -> Result<DcRevision, AicError> {
        self.revision.ok_or(AicError::CompletionMismatch)
    }

    fn firmware_images(&self) -> Result<crate::firmware::DcFirmwareImages, AicError> {
        let variant = match (self.revision()?, self.bluetooth_enabled) {
            (DcRevision::U02, _) => DcFirmwareVariant::U02,
            (DcRevision::HighPerformanceU02, false) => DcFirmwareVariant::HighPerformanceU02,
            (DcRevision::HighPerformanceU02, true) => {
                DcFirmwareVariant::HighPerformanceBluetoothU02
            }
        };
        Ok(dc_images(variant))
    }

    fn misc_ram_address(&self) -> Result<u32, AicError> {
        nonzero_address(self.misc_ram_address.unwrap_or(0))
    }

    fn config_address(&self, index: usize) -> Result<u32, AicError> {
        self.config_addresses
            .get(index)
            .copied()
            .ok_or(AicError::CompletionMismatch)
            .and_then(nonzero_address)
    }

    fn misc_ram_is_valid(&self) -> bool {
        self.misc_ram_words[0] == 0
            && self.misc_ram_words[1] & 0xfff0_0000 == 0x8000_0000
            && self.misc_ram_words[2] == 0
            && self.misc_ram_words[3] & 0xffff_ff00 == 0
    }

    fn dpd_word_address(&self, segment: DpdSegment, index: usize) -> Result<u32, AicError> {
        if index >= segment.word_count() {
            return Err(AicError::CompletionMismatch);
        }
        Ok(self.misc_ram_address()? + segment.offset() + index as u32 * 4)
    }

    fn install_dpd_word(
        &mut self,
        segment: DpdSegment,
        index: usize,
        value: u32,
    ) -> Result<(), AicError> {
        self.dpd_result
            .as_mut()
            .ok_or(AicError::CompletionMismatch)?
            .install_word(segment, index, value)
    }

    fn dpd_transfer(&self, segment: DpdSegment) -> Result<(u32, Vec<u8>), AicError> {
        let result = self
            .dpd_result
            .as_ref()
            .ok_or(AicError::CompletionMismatch)?;
        Ok((
            self.misc_ram_address()? + segment.offset(),
            result.segment_bytes(segment),
        ))
    }

    fn has_applicable_dpd_result(&self) -> bool {
        self.dpd_result
            .as_ref()
            .is_some_and(DpdCalibrationResult::should_apply)
    }
}

enum DcSystemOperation {
    Write { address: u32, value: u32 },
    MaskWrite { address: u32, mask: u32, value: u32 },
}

struct DcBlobTransfer {
    stage: DcStage,
    image: &'static [u8],
    address: u32,
    chunk_size: usize,
    offset: usize,
    next: DcStage,
}

impl AicDevice {
    pub(super) fn drive_dc_startup(&mut self, stage: DcStage, now: MonotonicTime) -> AicAction {
        match stage {
            DcStage::ReadSubId => self.begin_dc_read(DC_SUB_ID_ADDRESS, now),
            DcStage::ReadCrystalStatus => self.begin_dc_read(DC_CRYSTAL_STATUS_ADDRESS, now),
            DcStage::ReadBbpll => self.begin_dc_read(DC_BBPLL_CONFIG_ADDRESS, now),
            DcStage::WriteBbpll => {
                let value = match self
                    .dc_state()
                    .and_then(|state| state.bbpll_value.ok_or(AicError::CompletionMismatch))
                {
                    Ok(value) => value,
                    Err(error) => return self.fail(error),
                };
                self.begin_dc_write(DC_BBPLL_CONFIG_ADDRESS, value, now)
            }
            DcStage::SystemConfig(index) => self.drive_dc_system_config(index, now),
            DcStage::UploadPatch(offset) => {
                let image = match self.dc_images() {
                    Ok(images) => images.patch,
                    Err(error) => return self.fail(error),
                };
                self.drive_dc_blob(
                    DcBlobTransfer {
                        stage,
                        image,
                        address: DC_PATCH_ADDRESS,
                        chunk_size: FIRMWARE_UPLOAD_CHUNK,
                        offset,
                        next: DcStage::ReadMiscRamAddress,
                    },
                    now,
                )
            }
            DcStage::ReadMiscRamAddress => self.begin_dc_read(DC_CONFIG_BASE + 0x14, now),
            DcStage::ReadMiscRamWord(index) => {
                let address = match self.dc_state().and_then(DcStartupState::misc_ram_address) {
                    Ok(base) => base + index as u32 * 4,
                    Err(error) => return self.fail(error),
                };
                self.begin_dc_read(address, now)
            }
            DcStage::UploadCalibration(offset) => {
                let image = match self.dc_images() {
                    Ok(images) => images.calibration,
                    Err(error) => return self.fail(error),
                };
                self.drive_dc_blob(
                    DcBlobTransfer {
                        stage,
                        image,
                        address: DC_CALIBRATION_ADDRESS,
                        chunk_size: FIRMWARE_UPLOAD_CHUNK,
                        offset,
                        next: DcStage::RunCalibration,
                    },
                    now,
                )
            }
            DcStage::RunCalibration => {
                self.begin_debug_mailbox(
                    DBG_START_APP_REQ,
                    &start_app_payload(DC_CALIBRATION_ENTRY, HOST_START_APP_FNCALL),
                    now,
                );
                self.drive_mailbox(now)
            }
            DcStage::ReadDpdMiscRamAddress | DcStage::ReadRuntimeMiscRamAddress => {
                self.begin_dc_read(DC_CONFIG_BASE + 0x14, now)
            }
            DcStage::ReadDpdResult { segment, index } => {
                let address = match self
                    .dc_state()
                    .and_then(|state| state.dpd_word_address(segment, index))
                {
                    Ok(address) => address,
                    Err(error) => return self.fail(error),
                };
                self.begin_dc_read(address, now)
            }
            DcStage::ReadConfigPointer(index) => {
                let Some(offset) = DC_CONFIG_POINTER_OFFSETS.get(index) else {
                    return self.fail(AicError::CompletionMismatch);
                };
                self.begin_dc_read(DC_CONFIG_BASE + offset, now)
            }
            DcStage::WriteWifiSetting => {
                let address = match self.dc_state().and_then(|state| state.config_address(0)) {
                    Ok(base) => base + DC_WIFI_SETTING_OFFSET_U02,
                    Err(error) => return self.fail(error),
                };
                self.begin_dc_write(address, DC_WIFI_SETTING_U02, now)
            }
            DcStage::UploadLdpc(offset) => self.drive_dc_config_blob(
                stage,
                DC_LDPC_CONFIG,
                1,
                offset,
                now,
                DcStage::UploadAgc(0),
            ),
            DcStage::UploadAgc(offset) => self.drive_dc_config_blob(
                stage,
                DC_AGC_CONFIG,
                2,
                offset,
                now,
                DcStage::UploadTxGain(0),
            ),
            DcStage::UploadTxGain(offset) => {
                let image = match self.dc_state().and_then(DcStartupState::revision) {
                    Ok(DcRevision::U02) => DC_TX_GAIN_CONFIG,
                    Ok(DcRevision::HighPerformanceU02) => DC_TX_GAIN_CONFIG_H,
                    Err(error) => return self.fail(error),
                };
                self.drive_dc_config_blob(
                    stage,
                    image,
                    3,
                    offset,
                    now,
                    DcStage::WritePatchDescription,
                )
            }
            DcStage::WritePatchDescription => {
                let images = match self.dc_images() {
                    Ok(images) => images,
                    Err(error) => return self.fail(error),
                };
                let address = match patch_description_address(images.patch_table) {
                    Ok(address) => address,
                    Err(error) => return self.fail(error),
                };
                let payload = memory_block_write_payload(
                    address,
                    &images.patch_table[..DC_PATCH_DESCRIPTION_BYTES],
                );
                self.begin_debug_mailbox(DBG_MEM_BLOCK_WRITE_REQ, &payload, now);
                self.drive_mailbox(now)
            }
            DcStage::WritePatchEntry(offset) => {
                let images = match self.dc_images() {
                    Ok(images) => images,
                    Err(error) => return self.fail(error),
                };
                let Some((address, value)) = patch_entry(images.patch_table, offset) else {
                    self.set_startup_stage(StartupStage::Dc(DcStage::ReadBootAddress));
                    return self.drive_startup(now);
                };
                self.begin_dc_write(address, value, now)
            }
            DcStage::ReadBootAddress => self.begin_dc_read(DC_BOOT_ADDRESS, now),
            DcStage::ApplyDpd(segment) => {
                let (address, bytes) = match self
                    .dc_state()
                    .and_then(|state| state.dpd_transfer(segment))
                {
                    Ok(transfer) => transfer,
                    Err(error) => return self.fail(error),
                };
                let payload = memory_block_write_payload(address, &bytes);
                self.begin_debug_mailbox(DBG_MEM_BLOCK_WRITE_REQ, &payload, now);
                self.drive_mailbox(now)
            }
        }
    }

    pub(super) fn complete_dc_mailbox(
        &mut self,
        stage: DcStage,
        result: Vec<u8>,
    ) -> Result<StartupStage, AicError> {
        let next = match stage {
            DcStage::ReadSubId => {
                let sub_id = debug_memory_read(&result, DC_SUB_ID_ADDRESS)? as u8;
                self.dc_state_mut()?.install_sub_id(sub_id)?;
                let state = self.dc_state()?;
                log::info!(
                    "[wifi] AIC8800DC chip_id={:#04x} sub_id={} mcu_id={} bluetooth={} \
                     revision={:?}",
                    state.chip_id,
                    sub_id,
                    state.mcu_id,
                    state.bluetooth_enabled,
                    state.revision()?
                );
                DcStage::ReadCrystalStatus
            }
            DcStage::ReadCrystalStatus => {
                let value = debug_memory_read(&result, DC_CRYSTAL_STATUS_ADDRESS)?;
                if value & 1 == 0 {
                    DcStage::SystemConfig(0)
                } else {
                    DcStage::ReadBbpll
                }
            }
            DcStage::ReadBbpll => {
                let value = debug_memory_read(&result, DC_BBPLL_CONFIG_ADDRESS)?;
                if value >> 29 == 3 {
                    DcStage::SystemConfig(0)
                } else {
                    self.dc_state_mut()?.bbpll_value =
                        Some((value | (1 << 29) | (1 << 30)) & !(1 << 31));
                    DcStage::WriteBbpll
                }
            }
            DcStage::WriteBbpll => {
                let value = self
                    .dc_state()?
                    .bbpll_value
                    .ok_or(AicError::CompletionMismatch)?;
                require_debug_memory_write(&result, DC_BBPLL_CONFIG_ADDRESS, Some(value))?;
                DcStage::SystemConfig(0)
            }
            DcStage::SystemConfig(index) => {
                self.require_dc_system_config_confirmation(index, &result)?;
                DcStage::SystemConfig(index + 1)
            }
            DcStage::UploadPatch(offset) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                let length = self.dc_images()?.patch.len();
                DcStage::UploadPatch((offset + FIRMWARE_UPLOAD_CHUNK).min(length))
            }
            DcStage::ReadMiscRamAddress => {
                let address = nonzero_address(debug_memory_read(&result, DC_CONFIG_BASE + 0x14)?)?;
                self.dc_state_mut()?.misc_ram_address = Some(address);
                DcStage::ReadMiscRamWord(0)
            }
            DcStage::ReadMiscRamWord(index) => {
                let address = self.dc_state()?.misc_ram_address()? + index as u32 * 4;
                let value = debug_memory_read(&result, address)?;
                self.dc_state_mut()?.misc_ram_words[index] = value;
                if index == 3 {
                    if self.dc_state()?.misc_ram_is_valid() {
                        DcStage::ReadConfigPointer(0)
                    } else {
                        DcStage::UploadCalibration(0)
                    }
                } else {
                    DcStage::ReadMiscRamWord(index + 1)
                }
            }
            DcStage::UploadCalibration(offset) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                let length = self.dc_images()?.calibration.len();
                DcStage::UploadCalibration((offset + FIRMWARE_UPLOAD_CHUNK).min(length))
            }
            DcStage::RunCalibration => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_START_APP_REQ + 1, error))?;
                self.dc_state_mut()?.dpd_result = Some(DpdCalibrationResult::new());
                DcStage::ReadDpdMiscRamAddress
            }
            DcStage::ReadDpdMiscRamAddress => {
                let address = nonzero_address(debug_memory_read(&result, DC_CONFIG_BASE + 0x14)?)?;
                self.dc_state_mut()?.misc_ram_address = Some(address);
                DcStage::ReadDpdResult {
                    segment: DpdSegment::BitMask,
                    index: 0,
                }
            }
            DcStage::ReadDpdResult { segment, index } => {
                let address = self.dc_state()?.dpd_word_address(segment, index)?;
                let value = debug_memory_read(&result, address)?;
                self.dc_state_mut()?
                    .install_dpd_word(segment, index, value)?;
                if index + 1 < segment.word_count() {
                    DcStage::ReadDpdResult {
                        segment,
                        index: index + 1,
                    }
                } else if let Some(segment) = segment.next() {
                    DcStage::ReadDpdResult { segment, index: 0 }
                } else {
                    if !self.dc_state()?.has_applicable_dpd_result() {
                        log::warn!("[wifi] AIC DPD calibration returned no applicable result");
                        self.dc_state_mut()?.dpd_result = None;
                    }
                    DcStage::ReadConfigPointer(0)
                }
            }
            DcStage::ReadConfigPointer(index) => {
                let address = DC_CONFIG_BASE + DC_CONFIG_POINTER_OFFSETS[index];
                let value = nonzero_address(debug_memory_read(&result, address)?)?;
                self.dc_state_mut()?.config_addresses[index] = value;
                if index + 1 == DC_CONFIG_POINTER_OFFSETS.len() {
                    DcStage::WriteWifiSetting
                } else {
                    DcStage::ReadConfigPointer(index + 1)
                }
            }
            DcStage::WriteWifiSetting => {
                let address = self.dc_state()?.config_address(0)? + DC_WIFI_SETTING_OFFSET_U02;
                require_debug_memory_write(&result, address, Some(DC_WIFI_SETTING_U02))?;
                DcStage::UploadLdpc(0)
            }
            DcStage::UploadLdpc(offset) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                DcStage::UploadLdpc((offset + DC_CONFIG_UPLOAD_CHUNK).min(DC_LDPC_CONFIG.len()))
            }
            DcStage::UploadAgc(offset) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                DcStage::UploadAgc((offset + DC_CONFIG_UPLOAD_CHUNK).min(DC_AGC_CONFIG.len()))
            }
            DcStage::UploadTxGain(offset) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                let length = match self.dc_state()?.revision()? {
                    DcRevision::U02 => DC_TX_GAIN_CONFIG.len(),
                    DcRevision::HighPerformanceU02 => DC_TX_GAIN_CONFIG_H.len(),
                };
                DcStage::UploadTxGain((offset + DC_CONFIG_UPLOAD_CHUNK).min(length))
            }
            DcStage::WritePatchDescription => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                DcStage::WritePatchEntry(DC_PATCH_DESCRIPTION_BYTES)
            }
            DcStage::WritePatchEntry(offset) => {
                let images = self.dc_images()?;
                let (address, value) = patch_entry(images.patch_table, offset)
                    .ok_or(AicError::InvalidFirmwareAsset)?;
                require_debug_memory_write(&result, address, Some(value))?;
                DcStage::WritePatchEntry(offset + 8)
            }
            DcStage::ReadBootAddress => {
                let _ = debug_memory_read(&result, DC_BOOT_ADDRESS)?;
                return Ok(StartupStage::StartApplication);
            }
            DcStage::ReadRuntimeMiscRamAddress => {
                let address = nonzero_address(debug_memory_read(&result, DC_CONFIG_BASE + 0x14)?)?;
                self.dc_state_mut()?.misc_ram_address = Some(address);
                DcStage::ApplyDpd(DpdSegment::BitMask)
            }
            DcStage::ApplyDpd(segment) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                if let Some(segment) = segment.next() {
                    DcStage::ApplyDpd(segment)
                } else {
                    return Ok(StartupStage::StackStart);
                }
            }
        };
        Ok(StartupStage::Dc(next))
    }

    fn drive_dc_system_config(&mut self, index: usize, now: MonotonicTime) -> AicAction {
        let operation = match self
            .dc_state()
            .and_then(|state| dc_system_operation(index, state))
        {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                self.set_startup_stage(StartupStage::Dc(DcStage::UploadPatch(0)));
                return self.drive_startup(now);
            }
            Err(error) => return self.fail(error),
        };
        match operation {
            DcSystemOperation::Write { address, value } => self.begin_dc_write(address, value, now),
            DcSystemOperation::MaskWrite {
                address,
                mask,
                value,
            } => {
                self.begin_debug_mailbox(
                    DBG_MEM_MASK_WRITE_REQ,
                    &memory_mask_write_payload(address, mask, value),
                    now,
                );
                self.drive_mailbox(now)
            }
        }
    }

    fn require_dc_system_config_confirmation(
        &self,
        index: usize,
        result: &[u8],
    ) -> Result<(), AicError> {
        match dc_system_operation(index, self.dc_state()?)? {
            Some(DcSystemOperation::Write { address, value }) => {
                require_debug_memory_write(result, address, Some(value))?;
            }
            Some(DcSystemOperation::MaskWrite { address, .. }) => {
                require_debug_memory_write(result, address, None)?;
            }
            None => return Err(AicError::CompletionMismatch),
        }
        Ok(())
    }

    fn drive_dc_config_blob(
        &mut self,
        stage: DcStage,
        image: &'static [u8],
        config_index: usize,
        offset: usize,
        now: MonotonicTime,
        next: DcStage,
    ) -> AicAction {
        let address = match self
            .dc_state()
            .and_then(|state| state.config_address(config_index))
        {
            Ok(address) => address,
            Err(error) => return self.fail(error),
        };
        self.drive_dc_blob(
            DcBlobTransfer {
                stage,
                image,
                address,
                chunk_size: DC_CONFIG_UPLOAD_CHUNK,
                offset,
                next,
            },
            now,
        )
    }

    fn drive_dc_blob(&mut self, transfer: DcBlobTransfer, now: MonotonicTime) -> AicAction {
        if transfer.offset >= transfer.image.len() {
            self.set_startup_stage(StartupStage::Dc(transfer.next));
            return self.drive_startup(now);
        }
        let end = (transfer.offset + transfer.chunk_size).min(transfer.image.len());
        let payload = memory_block_write_payload(
            transfer.address + transfer.offset as u32,
            &transfer.image[transfer.offset..end],
        );
        self.set_startup_stage(StartupStage::Dc(transfer.stage));
        self.begin_debug_mailbox(DBG_MEM_BLOCK_WRITE_REQ, &payload, now);
        self.drive_mailbox(now)
    }

    fn begin_dc_read(&mut self, address: u32, now: MonotonicTime) -> AicAction {
        self.begin_debug_mailbox(DBG_MEM_READ_REQ, &memory_read_payload(address), now);
        self.drive_mailbox(now)
    }

    fn begin_dc_write(&mut self, address: u32, value: u32, now: MonotonicTime) -> AicAction {
        self.begin_debug_mailbox(
            DBG_MEM_WRITE_REQ,
            &memory_write_payload(address, value),
            now,
        );
        self.drive_mailbox(now)
    }

    fn dc_state(&self) -> Result<&DcStartupState, AicError> {
        self.lifecycle
            .startup
            .as_ref()
            .and_then(|startup| startup.dc.as_ref())
            .ok_or(AicError::CompletionMismatch)
    }

    fn dc_state_mut(&mut self) -> Result<&mut DcStartupState, AicError> {
        self.lifecycle
            .startup
            .as_mut()
            .and_then(|startup| startup.dc.as_mut())
            .ok_or(AicError::CompletionMismatch)
    }

    pub(super) fn dc_has_applicable_dpd_result(&self) -> bool {
        self.dc_state()
            .is_ok_and(DcStartupState::has_applicable_dpd_result)
    }

    fn dc_images(&self) -> Result<crate::firmware::DcFirmwareImages, AicError> {
        self.dc_state()?.firmware_images()
    }

    pub(super) fn dc_lmac_rf_payload(&self, index: u8) -> Result<Option<[u8; 260]>, AicError> {
        let revision = self.dc_state()?.revision()?;
        let table = match (index, revision) {
            (0, DcRevision::U02) => Some((RfTableSelection::Transmit, 0, &DC_TX_GAIN_TABLE_0[..])),
            (1, DcRevision::U02) => Some((RfTableSelection::Transmit, 16, &DC_TX_GAIN_TABLE_1[..])),
            (0, DcRevision::HighPerformanceU02) => {
                Some((RfTableSelection::Transmit, 0, &DC_TX_GAIN_TABLE_H_0[..]))
            }
            (1, DcRevision::HighPerformanceU02) => {
                Some((RfTableSelection::Transmit, 16, &DC_TX_GAIN_TABLE_H_1[..]))
            }
            (2, _) => Some((RfTableSelection::Receive, 0, &DC_RX_GAIN_TABLE_20M[..])),
            (3, _) => Some((RfTableSelection::Receive, 32, &DC_RX_GAIN_TABLE_40M[..])),
            _ => None,
        };
        table
            .map(|(selection, offset, words)| rf_config_payload(selection, offset, words))
            .transpose()
    }
}

fn dc_system_operation(
    index: usize,
    state: &DcStartupState,
) -> Result<Option<DcSystemOperation>, AicError> {
    if let Some(&(address, value)) = DC_SYSTEM_CONFIG.get(index) {
        return Ok(Some(DcSystemOperation::Write { address, value }));
    }
    let mut index = index - DC_SYSTEM_CONFIG.len();
    if state.mcu_id == 0 {
        if let Some(&(address, value)) = DC_MCU_SYSTEM_CONFIG_U02.get(index) {
            return Ok(Some(DcSystemOperation::Write { address, value }));
        }
        index = index.saturating_sub(DC_MCU_SYSTEM_CONFIG_U02.len());
    }
    let table = match state.revision()? {
        DcRevision::U02 => DC_MASKED_SYSTEM_CONFIG,
        DcRevision::HighPerformanceU02 => DC_MASKED_SYSTEM_CONFIG_H,
    };
    Ok(table.get(index).map(|&(address, mut mask, mut value)| {
        if address == 0x7000_1000 && state.mcu_id == 0 {
            mask |= (1 << 8) | (1 << 15);
            value |= (1 << 8) | (1 << 15);
        }
        DcSystemOperation::MaskWrite {
            address,
            mask,
            value,
        }
    }))
}

fn patch_description_address(table: &[u8]) -> Result<u32, AicError> {
    validate_patch_table(table)?;
    nonzero_address(u32::from_le_bytes(
        table[..4]
            .try_into()
            .map_err(|_| AicError::InvalidFirmwareAsset)?,
    ))
}

fn patch_entry(table: &[u8], offset: usize) -> Option<(u32, u32)> {
    let entry = table.get(offset..offset + 8)?;
    Some((
        u32::from_le_bytes(entry[..4].try_into().ok()?),
        u32::from_le_bytes(entry[4..].try_into().ok()?),
    ))
}

fn validate_patch_table(table: &[u8]) -> Result<(), AicError> {
    if table.len() < DC_PATCH_DESCRIPTION_BYTES
        || !(table.len() - DC_PATCH_DESCRIPTION_BYTES).is_multiple_of(8)
    {
        return Err(AicError::InvalidFirmwareAsset);
    }
    Ok(())
}

fn nonzero_address(address: u32) -> Result<u32, AicError> {
    (address != 0)
        .then_some(address)
        .ok_or(AicError::InvalidFirmwareAsset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ChipVariant;

    #[test]
    fn u01_and_inconsistent_h_identity_fail_closed() {
        let mut state = DcStartupState::from_chip_word(3 << 16).unwrap();
        assert_eq!(
            state.install_sub_id(0),
            Err(AicError::UnsupportedRevision(0))
        );
        assert_eq!(
            state.install_sub_id(2),
            Err(AicError::UnsupportedRevision(2))
        );
    }

    #[test]
    fn high_performance_bluetooth_identity_selects_hbt_firmware() {
        let mut state = DcStartupState::from_chip_word((0xc7_u32 << 16) | (1 << 26)).unwrap();
        state.install_sub_id(2).unwrap();

        let images = state.firmware_images().unwrap();
        assert_eq!(images.patch.len(), 21_116);
        assert_eq!(images.patch_table.len(), 568);
        assert_eq!(images.calibration.len(), 39_796);
    }

    #[test]
    fn pinned_patch_tables_have_strict_vendor_shape() {
        for variant in [
            DcFirmwareVariant::U02,
            DcFirmwareVariant::HighPerformanceU02,
            DcFirmwareVariant::HighPerformanceBluetoothU02,
        ] {
            let table = dc_images(variant).patch_table;
            assert_eq!(validate_patch_table(table), Ok(()));
            assert!(patch_description_address(table).unwrap() != 0);
        }
    }

    #[test]
    fn u02_validates_misc_ram_before_forced_dpd_calibration() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        let mut dc = DcStartupState::from_chip_word(3 << 16).unwrap();
        dc.install_sub_id(1).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(super::super::StartupState {
            stage: StartupStage::Dc(DcStage::ReadMiscRamAddress),
            revision: Some(3),
            dc: Some(dc),
        });
        let mut confirmation = Vec::new();
        confirmation.extend_from_slice(&(DC_CONFIG_BASE + 0x14).to_le_bytes());
        confirmation.extend_from_slice(&0x0011_02c0_u32.to_le_bytes());

        assert_eq!(
            device.complete_dc_mailbox(DcStage::ReadMiscRamAddress, confirmation),
            Ok(StartupStage::Dc(DcStage::ReadMiscRamWord(0)))
        );
        assert!(!dc_images(DcFirmwareVariant::U02).calibration.is_empty());
    }

    #[test]
    fn calibration_confirmation_does_not_jump_directly_to_patch_config() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        let mut dc = DcStartupState::from_chip_word(3 << 16).unwrap();
        dc.install_sub_id(1).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(super::super::StartupState {
            stage: StartupStage::Dc(DcStage::RunCalibration),
            revision: Some(3),
            dc: Some(dc),
        });

        assert_eq!(
            device.complete_dc_mailbox(DcStage::RunCalibration, [0; 4].to_vec()),
            Ok(StartupStage::Dc(DcStage::ReadDpdMiscRamAddress))
        );
    }

    #[test]
    fn dpd_segments_follow_the_vendor_misc_ram_layout() {
        let mut result = DpdCalibrationResult::new();
        result.install_word(DpdSegment::BitMask, 1, 1).unwrap();
        result.install_word(DpdSegment::High, 95, 2).unwrap();
        result.install_word(DpdSegment::Loft, 17, 3).unwrap();

        assert_eq!(DpdSegment::BitMask.offset(), 0);
        assert_eq!(DpdSegment::High.offset(), 16);
        assert_eq!(DpdSegment::Loft.offset(), 0x06d0);
        assert_eq!(result.segment_bytes(DpdSegment::BitMask).len(), 16);
        assert_eq!(result.segment_bytes(DpdSegment::High).len(), 384);
        assert_eq!(result.segment_bytes(DpdSegment::Loft).len(), 72);
        assert!(result.should_apply());
    }

    #[test]
    fn mcu_system_config_adds_vendor_power_bits() {
        let mut state = DcStartupState::from_chip_word((3 << 16) | (1 << 25)).unwrap();
        state.install_sub_id(1).unwrap();
        let index = DC_SYSTEM_CONFIG.len()
            + DC_MCU_SYSTEM_CONFIG_U02.len()
            + DC_MASKED_SYSTEM_CONFIG
                .iter()
                .position(|entry| entry.0 == 0x7000_1000)
                .unwrap();
        let Some(DcSystemOperation::MaskWrite { mask, value, .. }) =
            dc_system_operation(index, &state).unwrap()
        else {
            panic!("expected the masked MCU configuration")
        };
        assert_ne!(mask & ((1 << 8) | (1 << 15)), 0);
        assert_ne!(value & ((1 << 8) | (1 << 15)), 0);
    }

    #[test]
    fn revision_read_uses_the_vendor_chip_address() {
        assert_eq!(crate::common::CHIP_REV_ADDR, 0x4050_0000);
    }
}
