//! Firmware-independent startup confirmations and the D80 image upload path.

use alloc::vec::Vec;

use super::{
    START_STABILIZE, StartupStage,
    dc::{DcStage, DcStartupState},
    map_debug_error,
};
use crate::{
    common::{CHIP_REV_ADDR, CHIP_REV_HIGH_SHIFT, CHIP_REV_MASK},
    device::*,
    firmware::{D80_MAIN_ADDRESS, FIRMWARE_UPLOAD_CHUNK, d80_main_image},
    lmac,
    profile::FirmwareProfile,
    protocol::{
        DBG_MEM_BLOCK_WRITE_REQ, DBG_MEM_READ_REQ, DBG_MEM_WRITE_REQ, DBG_START_APP_REQ,
        debug_memory_read, memory_block_write_payload, memory_read_payload, memory_write_payload,
        require_debug_memory_write, require_debug_status,
    },
};

/// Offsets and values of the vendor D80 patch configuration written into the
/// uploaded image before the application starts (`aicwifi_patch_config_8800d80`).
const D80_PATCH_CONFIG_OFFSET: u32 = 0x0198;
const D80_PATCH_VERSION_OFFSET: u32 = 0x01c;
const D80_PATCH_START_ADDR: u32 = 0x0016_F800;
const D80_PATCH_VERSION_THRESHOLD: u32 = 0x0609_0100;
const D80_PATCH_PAIR_BYTES: u32 = 8;
const D80_PATCH_BLOCK_COUNT: usize = 4;
const D80_PATCH_MAGIC_OFFSET: u32 = 0;
const D80_PATCH_MAGIC_2_OFFSET: u32 = 8;
const D80_PATCH_PAIR_START_OFFSET: u32 = 4;
const D80_PATCH_PAIR_COUNT_OFFSET: u32 = 12;
const D80_PATCH_BLOCK_SIZE_OFFSET: u32 = 48;
const D80_PATCH_MAGIC: u32 = 0x4843_5450; // "PTCH"
const D80_PATCH_MAGIC_2: u32 = 0x5054_4348; // "HCTP"
const D80_PATCH_PAIRS: &[[u32; 2]] = &[
    [0x00b4, 0xf301_0000], // 2.4 GHz only (USE_5G=0)
    [0x0170, 0x0100_000a], // AMSDU_RX
    [0x0188, 0x0000_0003], // user_ext_flags: PWROFST_COVER_CALIB | USER_CHAN_MAX_TXPWR_EN
];

/// Steps of the D80 patch configuration sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum D80PatchStage {
    ReadConfigBase,
    ReadStructBase {
        config_base: u32,
    },
    ReadVersion {
        config_base: u32,
        struct_base: u32,
    },
    ReadPatchBuffer {
        config_base: u32,
        struct_base: u32,
        version: u32,
    },
    WriteMagic {
        config_base: u32,
        struct_base: u32,
        start_addr: u32,
    },
    WriteMagic2 {
        config_base: u32,
        struct_base: u32,
        start_addr: u32,
    },
    WritePairBase {
        config_base: u32,
        struct_base: u32,
        start_addr: u32,
    },
    WritePairCount {
        config_base: u32,
        struct_base: u32,
        start_addr: u32,
    },
    WritePairOffset {
        config_base: u32,
        struct_base: u32,
        start_addr: u32,
        index: usize,
    },
    WritePairValue {
        config_base: u32,
        struct_base: u32,
        start_addr: u32,
        index: usize,
    },
    WriteBlockSize {
        struct_base: u32,
        index: usize,
    },
}

impl AicDevice {
    pub(super) fn drive_main_upload(&mut self, offset: usize, now: MonotonicTime) -> AicAction {
        let main = d80_main_image();
        if offset >= main.len() {
            self.set_startup_stage(StartupStage::D80Patch(D80PatchStage::ReadConfigBase));
            return self.drive_startup(now);
        }
        let end = (offset + FIRMWARE_UPLOAD_CHUNK).min(main.len());
        let payload = memory_block_write_payload(
            D80_MAIN_ADDRESS.wrapping_add(offset as u32),
            &main[offset..end],
        );
        self.begin_debug_mailbox(DBG_MEM_BLOCK_WRITE_REQ, &payload, now);
        self.drive_mailbox(now)
    }

    pub(super) fn drive_d80_patch(
        &mut self,
        stage: D80PatchStage,
        now: MonotonicTime,
    ) -> AicAction {
        match stage {
            D80PatchStage::ReadConfigBase => {
                self.begin_debug_mailbox(
                    DBG_MEM_READ_REQ,
                    &memory_read_payload(D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET),
                    now,
                );
                self.drive_mailbox(now)
            }
            D80PatchStage::ReadStructBase { .. } => {
                self.begin_debug_mailbox(
                    DBG_MEM_READ_REQ,
                    &memory_read_payload(D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 8),
                    now,
                );
                self.drive_mailbox(now)
            }
            D80PatchStage::ReadVersion { .. } => {
                self.begin_debug_mailbox(
                    DBG_MEM_READ_REQ,
                    &memory_read_payload(D80_MAIN_ADDRESS + D80_PATCH_VERSION_OFFSET),
                    now,
                );
                self.drive_mailbox(now)
            }
            D80PatchStage::ReadPatchBuffer { .. } => {
                self.begin_debug_mailbox(
                    DBG_MEM_READ_REQ,
                    &memory_read_payload(D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 12),
                    now,
                );
                self.drive_mailbox(now)
            }
            D80PatchStage::WriteMagic { struct_base, .. } => {
                self.d80_patch_write(struct_base + D80_PATCH_MAGIC_OFFSET, D80_PATCH_MAGIC, now)
            }
            D80PatchStage::WriteMagic2 { struct_base, .. } => self.d80_patch_write(
                struct_base + D80_PATCH_MAGIC_2_OFFSET,
                D80_PATCH_MAGIC_2,
                now,
            ),
            D80PatchStage::WritePairBase {
                struct_base,
                start_addr,
                ..
            } => self.d80_patch_write(struct_base + D80_PATCH_PAIR_START_OFFSET, start_addr, now),
            D80PatchStage::WritePairCount { struct_base, .. } => self.d80_patch_write(
                struct_base + D80_PATCH_PAIR_COUNT_OFFSET,
                D80_PATCH_PAIRS.len() as u32,
                now,
            ),
            D80PatchStage::WritePairOffset {
                config_base,
                start_addr,
                index,
                ..
            } => self.d80_patch_write(
                start_addr + index as u32 * D80_PATCH_PAIR_BYTES,
                D80_PATCH_PAIRS[index][0].wrapping_add(config_base),
                now,
            ),
            D80PatchStage::WritePairValue {
                start_addr, index, ..
            } => self.d80_patch_write(
                start_addr + index as u32 * D80_PATCH_PAIR_BYTES + 4,
                D80_PATCH_PAIRS[index][1],
                now,
            ),
            D80PatchStage::WriteBlockSize { struct_base, index } => self.d80_patch_write(
                struct_base + D80_PATCH_BLOCK_SIZE_OFFSET + index as u32 * 4,
                0,
                now,
            ),
        }
    }

    fn d80_patch_write(&mut self, address: u32, value: u32, now: MonotonicTime) -> AicAction {
        self.begin_debug_mailbox(
            DBG_MEM_WRITE_REQ,
            &memory_write_payload(address, value),
            now,
        );
        self.drive_mailbox(now)
    }

    fn complete_d80_patch_mailbox(
        &mut self,
        stage: D80PatchStage,
        result: Vec<u8>,
    ) -> Result<StartupStage, AicError> {
        Ok(match stage {
            D80PatchStage::ReadConfigBase => {
                let config_base =
                    debug_memory_read(&result, D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET)?;
                StartupStage::D80Patch(D80PatchStage::ReadStructBase { config_base })
            }
            D80PatchStage::ReadStructBase { config_base } => {
                let struct_base =
                    debug_memory_read(&result, D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 8)?;
                StartupStage::D80Patch(D80PatchStage::ReadVersion {
                    config_base,
                    struct_base,
                })
            }
            D80PatchStage::ReadVersion {
                config_base,
                struct_base,
            } => {
                let version =
                    debug_memory_read(&result, D80_MAIN_ADDRESS + D80_PATCH_VERSION_OFFSET)?;
                log::info!("[wifi] D80 firmware version {version:#010x}");
                if version > D80_PATCH_VERSION_THRESHOLD {
                    StartupStage::D80Patch(D80PatchStage::ReadPatchBuffer {
                        config_base,
                        struct_base,
                        version,
                    })
                } else {
                    StartupStage::D80Patch(D80PatchStage::WriteMagic {
                        config_base,
                        struct_base,
                        start_addr: D80_PATCH_START_ADDR,
                    })
                }
            }
            D80PatchStage::ReadPatchBuffer {
                config_base,
                struct_base,
                version,
            } => {
                let start_addr =
                    debug_memory_read(&result, D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 12)?;
                log::info!(
                    "[wifi] D80 patch buffer relocated to {start_addr:#010x} (firmware \
                     {version:#010x})"
                );
                StartupStage::D80Patch(D80PatchStage::WriteMagic {
                    config_base,
                    struct_base,
                    start_addr,
                })
            }
            D80PatchStage::WriteMagic {
                config_base,
                struct_base,
                start_addr,
            } => {
                require_debug_memory_write(
                    &result,
                    struct_base + D80_PATCH_MAGIC_OFFSET,
                    Some(D80_PATCH_MAGIC),
                )?;
                StartupStage::D80Patch(D80PatchStage::WriteMagic2 {
                    config_base,
                    struct_base,
                    start_addr,
                })
            }
            D80PatchStage::WriteMagic2 {
                config_base,
                struct_base,
                start_addr,
            } => {
                require_debug_memory_write(
                    &result,
                    struct_base + D80_PATCH_MAGIC_2_OFFSET,
                    Some(D80_PATCH_MAGIC_2),
                )?;
                StartupStage::D80Patch(D80PatchStage::WritePairBase {
                    config_base,
                    struct_base,
                    start_addr,
                })
            }
            D80PatchStage::WritePairBase {
                config_base,
                struct_base,
                start_addr,
            } => {
                require_debug_memory_write(
                    &result,
                    struct_base + D80_PATCH_PAIR_START_OFFSET,
                    Some(start_addr),
                )?;
                StartupStage::D80Patch(D80PatchStage::WritePairCount {
                    config_base,
                    struct_base,
                    start_addr,
                })
            }
            D80PatchStage::WritePairCount {
                config_base,
                struct_base,
                start_addr,
            } => {
                require_debug_memory_write(
                    &result,
                    struct_base + D80_PATCH_PAIR_COUNT_OFFSET,
                    Some(D80_PATCH_PAIRS.len() as u32),
                )?;
                StartupStage::D80Patch(D80PatchStage::WritePairOffset {
                    config_base,
                    struct_base,
                    start_addr,
                    index: 0,
                })
            }
            D80PatchStage::WritePairOffset {
                config_base,
                struct_base,
                start_addr,
                index,
            } => {
                let offset = D80_PATCH_PAIRS[index][0].wrapping_add(config_base);
                require_debug_memory_write(
                    &result,
                    start_addr + index as u32 * D80_PATCH_PAIR_BYTES,
                    Some(offset),
                )?;
                StartupStage::D80Patch(D80PatchStage::WritePairValue {
                    config_base,
                    struct_base,
                    start_addr,
                    index,
                })
            }
            D80PatchStage::WritePairValue {
                config_base,
                struct_base,
                start_addr,
                index,
            } => {
                require_debug_memory_write(
                    &result,
                    start_addr + index as u32 * D80_PATCH_PAIR_BYTES + 4,
                    Some(D80_PATCH_PAIRS[index][1]),
                )?;
                if index + 1 < D80_PATCH_PAIRS.len() {
                    StartupStage::D80Patch(D80PatchStage::WritePairOffset {
                        config_base,
                        struct_base,
                        start_addr,
                        index: index + 1,
                    })
                } else {
                    StartupStage::D80Patch(D80PatchStage::WriteBlockSize {
                        struct_base,
                        index: 0,
                    })
                }
            }
            D80PatchStage::WriteBlockSize { struct_base, index } => {
                require_debug_memory_write(
                    &result,
                    struct_base + D80_PATCH_BLOCK_SIZE_OFFSET + index as u32 * 4,
                    Some(0),
                )?;
                if index + 1 < D80_PATCH_BLOCK_COUNT {
                    StartupStage::D80Patch(D80PatchStage::WriteBlockSize {
                        struct_base,
                        index: index + 1,
                    })
                } else {
                    log::info!(
                        "[wifi] D80 patch config applied ({} entries)",
                        D80_PATCH_PAIRS.len()
                    );
                    StartupStage::StartApplication
                }
            }
        })
    }

    pub(in crate::device) fn complete_startup_mailbox(
        &mut self,
        result: Vec<u8>,
    ) -> Result<(), AicError> {
        let stage = self
            .lifecycle
            .startup
            .as_ref()
            .map(|startup| startup.stage)
            .ok_or(AicError::CompletionMismatch)?;
        let next = match stage {
            StartupStage::ReadRevision => self.complete_revision_read(&result)?,
            StartupStage::UploadMain(offset) => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_MEM_BLOCK_WRITE_REQ + 1, error))?;
                let length = d80_main_image().len();
                let next = (offset + FIRMWARE_UPLOAD_CHUNK).min(length);
                if next >= length {
                    StartupStage::D80Patch(D80PatchStage::ReadConfigBase)
                } else {
                    StartupStage::UploadMain(next)
                }
            }
            StartupStage::D80Patch(stage) => self.complete_d80_patch_mailbox(stage, result)?,
            StartupStage::Dc(stage) => self.complete_dc_mailbox(stage, result)?,
            StartupStage::StartApplication => {
                require_debug_status(&result)
                    .map_err(|error| map_debug_error(DBG_START_APP_REQ + 1, error))?;
                self.lifecycle.retry_at = Some(self.lifecycle.last_time.after(START_STABILIZE));
                StartupStage::Stabilize
            }
            StartupStage::StackStart => {
                if result.len() != 2 {
                    return Err(AicError::MalformedResponse);
                }
                match self.firmware_profile() {
                    FirmwareProfile::Aic8800Dc => StartupStage::RfConfig(0),
                    FirmwareProfile::Aic8800D80 => StartupStage::TxPowerLevel,
                }
            }
            StartupStage::TxPowerLevel => {
                lmac::require_empty(lmac::MM_SET_TXPWR_IDX_LVL_CFM, &result)?;
                StartupStage::RfCalibration
            }
            StartupStage::RfConfig(index) => {
                lmac::require_empty(lmac::MM_SET_RF_CONFIG_CFM, &result)?;
                if index == 3 {
                    StartupStage::RfCalibration
                } else {
                    StartupStage::RfConfig(index + 1)
                }
            }
            StartupStage::RfCalibration => {
                match self.firmware_profile() {
                    FirmwareProfile::Aic8800Dc => {
                        lmac::require_empty(lmac::MM_SET_RF_CALIB_CFM, &result)?;
                    }
                    FirmwareProfile::Aic8800D80 if result.len() == 16 => {}
                    FirmwareProfile::Aic8800D80 => return Err(AicError::MalformedResponse),
                }
                StartupStage::ReadMacAddress
            }
            StartupStage::ReadMacAddress => {
                self.data.link.install_mac(lmac::parse_mac(&result)?)?;
                StartupStage::FirmwareReset
            }
            StartupStage::FirmwareReset => {
                lmac::require_empty(lmac::MM_RESET_CFM, &result)?;
                StartupStage::ConfigureMac
            }
            StartupStage::ConfigureMac => {
                lmac::require_empty(lmac::ME_CONFIG_CFM, &result)?;
                StartupStage::ConfigureChannels
            }
            StartupStage::ConfigureChannels => {
                lmac::require_empty(lmac::ME_CHAN_CONFIG_CFM, &result)?;
                StartupStage::AddStationInterface
            }
            StartupStage::AddStationInterface => {
                let index = lmac::parse_add_interface(&result)?;
                self.data.link.install_interface(index)?;
                StartupStage::StartMac
            }
            StartupStage::StartMac => {
                lmac::require_empty(lmac::MM_START_CFM, &result)?;
                StartupStage::SetFilter
            }
            StartupStage::SetFilter => {
                lmac::require_empty(lmac::MM_SET_FILTER_CFM, &result)?;
                StartupStage::ArmChipInterrupt
            }
            _ => return Err(AicError::CompletionMismatch),
        };
        self.lifecycle
            .startup
            .as_mut()
            .expect("startup state was checked above")
            .stage = next;
        Ok(())
    }

    fn complete_revision_read(&mut self, result: &[u8]) -> Result<StartupStage, AicError> {
        let raw = debug_memory_read(result, CHIP_REV_ADDR)?;
        let revision = ((raw >> CHIP_REV_HIGH_SHIFT) & CHIP_REV_MASK) as u8;
        if !matches!(revision, 1 | 3 | 7) {
            return Err(AicError::UnsupportedRevision(revision));
        }
        let profile = self.firmware_profile();
        let startup = self
            .lifecycle
            .startup
            .as_mut()
            .ok_or(AicError::CompletionMismatch)?;
        startup.revision = Some(revision);
        Ok(match profile {
            FirmwareProfile::Aic8800Dc => {
                startup.dc = Some(DcStartupState::from_chip_word(raw)?);
                StartupStage::Dc(DcStage::ReadSubId)
            }
            FirmwareProfile::Aic8800D80 => StartupStage::UploadMain(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ChipVariant;

    #[test]
    fn dc_revision_enters_the_dc_owner_state_machine() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(super::super::StartupState::new());
        device.set_startup_stage(StartupStage::ReadRevision);
        let mut confirmation = Vec::new();
        confirmation.extend_from_slice(&CHIP_REV_ADDR.to_le_bytes());
        confirmation.extend_from_slice(&(3u32 << 16).to_le_bytes());

        assert_eq!(device.complete_startup_mailbox(confirmation), Ok(()));
        assert!(matches!(
            device.lifecycle.startup.as_ref().map(|state| state.stage),
            Some(StartupStage::Dc(DcStage::ReadSubId))
        ));
    }

    #[test]
    fn dc_empty_rf_calibration_confirmation_advances_to_mac_read() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(super::super::StartupState::new());
        device.set_startup_stage(StartupStage::RfCalibration);

        assert_eq!(device.complete_startup_mailbox(Vec::new()), Ok(()));
        assert!(matches!(
            device.lifecycle.startup.as_ref().map(|state| state.stage),
            Some(StartupStage::ReadMacAddress)
        ));
    }

    #[test]
    fn d80_upload_completion_begins_patch_configuration_before_starting_the_app() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(super::super::StartupState::new());
        device.set_startup_stage(StartupStage::UploadMain(
            d80_main_image().len() - FIRMWARE_UPLOAD_CHUNK,
        ));

        assert_eq!(device.complete_startup_mailbox([0; 4].to_vec()), Ok(()));
        let AicAction::SubmitSdio(flow) = device.drive_startup(MonotonicTime::from_nanos(0)) else {
            panic!("expected the mailbox flow read")
        };
        let AicAction::SubmitSdio(write) = device.advance(AicInput {
            now: MonotonicTime::from_nanos(0),
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: flow.id,
                result: Ok(SdioResponse::Byte(64)),
            })),
        }) else {
            panic!("expected the mailbox FIFO write")
        };

        // The firmware header read that feeds the patch config must run before
        // the start-application mailbox; the vendor sequence never starts the
        // app immediately after the image upload.
        let SdioRequestKind::Write { bytes, .. } = write.kind else {
            panic!("expected a FIFO write")
        };
        assert_eq!(&bytes[8..10], &DBG_MEM_READ_REQ.to_le_bytes());
        assert_eq!(&bytes[16..20], &(D80_MAIN_ADDRESS + 0x0198).to_le_bytes());
    }

    fn debug_confirmation(address: u32, value: u32) -> Vec<u8> {
        let mut confirmation = Vec::new();
        confirmation.extend_from_slice(&address.to_le_bytes());
        confirmation.extend_from_slice(&value.to_le_bytes());
        confirmation
    }

    /// Drives the current patch stage through the credit gate and returns the
    /// bytes of the mailbox FIFO write it emits.
    fn drive_patch_mailbox(device: &mut AicDevice) -> Vec<u8> {
        device.lifecycle.mailbox = None;
        let AicAction::SubmitSdio(flow) = device.drive_startup(MonotonicTime::from_nanos(0)) else {
            panic!("expected the mailbox flow read")
        };
        let AicAction::SubmitSdio(write) = device.advance(AicInput {
            now: MonotonicTime::from_nanos(0),
            event: Some(AicInputEvent::Sdio(SdioCompletion {
                request_id: flow.id,
                result: Ok(SdioResponse::Byte(64)),
            })),
        }) else {
            panic!("expected the mailbox FIFO write")
        };
        let SdioRequestKind::Write { bytes, .. } = write.kind else {
            panic!("expected a FIFO write")
        };
        bytes
    }

    fn read_request(device: &mut AicDevice, stage: D80PatchStage) -> u32 {
        device.set_startup_stage(StartupStage::D80Patch(stage));
        let bytes = drive_patch_mailbox(device);
        assert_eq!(&bytes[8..10], &DBG_MEM_READ_REQ.to_le_bytes());
        u32::from_le_bytes(bytes[16..20].try_into().expect("four-byte address"))
    }

    fn write_request(device: &mut AicDevice, stage: D80PatchStage) -> (u32, u32) {
        device.set_startup_stage(StartupStage::D80Patch(stage));
        let bytes = drive_patch_mailbox(device);
        assert_eq!(&bytes[8..10], &DBG_MEM_WRITE_REQ.to_le_bytes());
        (
            u32::from_le_bytes(bytes[16..20].try_into().expect("four-byte address")),
            u32::from_le_bytes(bytes[20..24].try_into().expect("four-byte value")),
        )
    }

    fn patch_device() -> AicDevice {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(super::super::StartupState::new());
        device
    }

    #[test]
    fn d80_patch_config_uses_the_fixed_patch_buffer_for_legacy_firmware() {
        let mut device = patch_device();

        assert_eq!(
            read_request(&mut device, D80PatchStage::ReadConfigBase),
            D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET,
                0x0002_0000,
            )),
            Ok(())
        );
        assert_eq!(
            read_request(
                &mut device,
                D80PatchStage::ReadStructBase {
                    config_base: 0x0002_0000,
                },
            ),
            D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 8
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 8,
                0x0016_e000,
            )),
            Ok(())
        );
        assert_eq!(
            read_request(
                &mut device,
                D80PatchStage::ReadVersion {
                    config_base: 0x0002_0000,
                    struct_base: 0x0016_e000,
                },
            ),
            D80_MAIN_ADDRESS + D80_PATCH_VERSION_OFFSET
        );
        // Firmware at exactly the threshold keeps the fixed patch buffer.
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                D80_MAIN_ADDRESS + D80_PATCH_VERSION_OFFSET,
                D80_PATCH_VERSION_THRESHOLD,
            )),
            Ok(())
        );
        assert!(matches!(
            device.lifecycle.startup.as_ref().map(|state| state.stage),
            Some(StartupStage::D80Patch(D80PatchStage::WriteMagic {
                config_base: 0x0002_0000,
                struct_base: 0x0016_e000,
                start_addr: D80_PATCH_START_ADDR,
            }))
        ));
    }

    #[test]
    fn d80_patch_config_relocates_the_patch_buffer_for_newer_firmware() {
        let mut device = patch_device();
        device.set_startup_stage(StartupStage::D80Patch(D80PatchStage::ReadVersion {
            config_base: 0x0002_0000,
            struct_base: 0x0016_e000,
        }));

        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                D80_MAIN_ADDRESS + D80_PATCH_VERSION_OFFSET,
                D80_PATCH_VERSION_THRESHOLD + 1,
            )),
            Ok(())
        );
        assert_eq!(
            read_request(
                &mut device,
                D80PatchStage::ReadPatchBuffer {
                    config_base: 0x0002_0000,
                    struct_base: 0x0016_e000,
                    version: D80_PATCH_VERSION_THRESHOLD + 1,
                },
            ),
            D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 12
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                D80_MAIN_ADDRESS + D80_PATCH_CONFIG_OFFSET + 12,
                0x0017_0000,
            )),
            Ok(())
        );
        assert!(matches!(
            device.lifecycle.startup.as_ref().map(|state| state.stage),
            Some(StartupStage::D80Patch(D80PatchStage::WriteMagic {
                start_addr: 0x0017_0000,
                ..
            }))
        ));
    }

    #[test]
    fn d80_patch_config_writes_the_vendor_patch_structure() {
        const CONFIG_BASE: u32 = 0x0002_0000;
        const STRUCT_BASE: u32 = 0x0016_e000;
        const START_ADDR: u32 = D80_PATCH_START_ADDR;
        let mut device = patch_device();

        assert_eq!(
            write_request(
                &mut device,
                D80PatchStage::WriteMagic {
                    config_base: CONFIG_BASE,
                    struct_base: STRUCT_BASE,
                    start_addr: START_ADDR,
                },
            ),
            (STRUCT_BASE + D80_PATCH_MAGIC_OFFSET, D80_PATCH_MAGIC)
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                STRUCT_BASE + D80_PATCH_MAGIC_OFFSET,
                D80_PATCH_MAGIC,
            )),
            Ok(())
        );
        assert_eq!(
            write_request(
                &mut device,
                D80PatchStage::WriteMagic2 {
                    config_base: CONFIG_BASE,
                    struct_base: STRUCT_BASE,
                    start_addr: START_ADDR,
                },
            ),
            (STRUCT_BASE + D80_PATCH_MAGIC_2_OFFSET, D80_PATCH_MAGIC_2)
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                STRUCT_BASE + D80_PATCH_MAGIC_2_OFFSET,
                D80_PATCH_MAGIC_2,
            )),
            Ok(())
        );
        assert_eq!(
            write_request(
                &mut device,
                D80PatchStage::WritePairBase {
                    config_base: CONFIG_BASE,
                    struct_base: STRUCT_BASE,
                    start_addr: START_ADDR,
                },
            ),
            (STRUCT_BASE + D80_PATCH_PAIR_START_OFFSET, START_ADDR)
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                STRUCT_BASE + D80_PATCH_PAIR_START_OFFSET,
                START_ADDR,
            )),
            Ok(())
        );
        assert_eq!(
            write_request(
                &mut device,
                D80PatchStage::WritePairCount {
                    config_base: CONFIG_BASE,
                    struct_base: STRUCT_BASE,
                    start_addr: START_ADDR,
                },
            ),
            (STRUCT_BASE + D80_PATCH_PAIR_COUNT_OFFSET, 3)
        );
        assert_eq!(
            device.complete_startup_mailbox(debug_confirmation(
                STRUCT_BASE + D80_PATCH_PAIR_COUNT_OFFSET,
                3,
            )),
            Ok(())
        );

        for (index, [offset, value]) in D80_PATCH_PAIRS.iter().enumerate() {
            assert_eq!(
                write_request(
                    &mut device,
                    D80PatchStage::WritePairOffset {
                        config_base: CONFIG_BASE,
                        struct_base: STRUCT_BASE,
                        start_addr: START_ADDR,
                        index,
                    },
                ),
                (
                    START_ADDR + index as u32 * D80_PATCH_PAIR_BYTES,
                    offset.wrapping_add(CONFIG_BASE),
                )
            );
            assert_eq!(
                device.complete_startup_mailbox(debug_confirmation(
                    START_ADDR + index as u32 * D80_PATCH_PAIR_BYTES,
                    offset.wrapping_add(CONFIG_BASE),
                )),
                Ok(())
            );
            assert_eq!(
                write_request(
                    &mut device,
                    D80PatchStage::WritePairValue {
                        config_base: CONFIG_BASE,
                        struct_base: STRUCT_BASE,
                        start_addr: START_ADDR,
                        index,
                    },
                ),
                (START_ADDR + index as u32 * D80_PATCH_PAIR_BYTES + 4, *value)
            );
            assert_eq!(
                device.complete_startup_mailbox(debug_confirmation(
                    START_ADDR + index as u32 * D80_PATCH_PAIR_BYTES + 4,
                    *value,
                )),
                Ok(())
            );
        }

        for index in 0..D80_PATCH_BLOCK_COUNT {
            assert_eq!(
                write_request(
                    &mut device,
                    D80PatchStage::WriteBlockSize {
                        struct_base: STRUCT_BASE,
                        index,
                    },
                ),
                (
                    STRUCT_BASE + D80_PATCH_BLOCK_SIZE_OFFSET + index as u32 * 4,
                    0
                )
            );
            assert_eq!(
                device.complete_startup_mailbox(debug_confirmation(
                    STRUCT_BASE + D80_PATCH_BLOCK_SIZE_OFFSET + index as u32 * 4,
                    0,
                )),
                Ok(())
            );
        }

        assert!(matches!(
            device.lifecycle.startup.as_ref().map(|state| state.stage),
            Some(StartupStage::StartApplication)
        ));
    }

    #[test]
    fn d80_patch_write_confirmation_rejects_a_mismatched_echo() {
        let mut device = patch_device();
        device.set_startup_stage(StartupStage::D80Patch(D80PatchStage::WriteMagic {
            config_base: 0x0002_0000,
            struct_base: 0x0016_e000,
            start_addr: D80_PATCH_START_ADDR,
        }));

        assert!(matches!(
            device.complete_startup_mailbox(debug_confirmation(0x0016_e000, D80_PATCH_MAGIC ^ 1,)),
            Err(AicError::MalformedResponse)
        ));
    }
}
