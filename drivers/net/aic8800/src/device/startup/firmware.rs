//! Firmware-independent startup confirmations and the D80 image upload path.

use alloc::vec::Vec;

use super::{
    START_STABILIZE, StartupStage,
    d80::D80PatchStage,
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
        DBG_MEM_BLOCK_WRITE_REQ, DBG_START_APP_REQ, debug_memory_read, memory_block_write_payload,
        require_debug_status,
    },
};

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
    use crate::{common::ChipVariant, protocol::DBG_MEM_READ_REQ};

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
}
