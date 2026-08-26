use alloc::vec::Vec;

use super::{START_STABILIZE, StartupStage};
use crate::{
    common::{CHIP_REV_MASK, ChipVariant},
    device::*,
    firmware::{
        CONFIG_BASE_OFFSET, MAIN_ADDRESS, MASKED_SYSTEM_CONFIG, PATCH_ADDRESS,
        PATCH_ADDRESS_REGISTER, PATCH_COUNT_REGISTER, PATCH_TABLE, PATCH_TABLE_ADDRESS,
        SYSTEM_CONFIG, UPLOAD_CHUNK, images,
    },
    protocol::{
        DBG_MEM_BLOCK_WRITE_REQ, DBG_MEM_MASK_WRITE_REQ, DBG_MEM_READ_REQ, DBG_MEM_WRITE_REQ,
        memory_block_write_payload, memory_mask_write_payload, memory_read_payload,
        memory_write_payload,
    },
};

impl AicDevice {
    pub(super) fn drive_system_config(&mut self, index: usize, now: MonotonicTime) -> AicAction {
        if let Some(&(address, value)) = SYSTEM_CONFIG.get(index) {
            self.begin_debug_mailbox(
                DBG_MEM_WRITE_REQ,
                &memory_write_payload(address, value),
                now,
            );
            self.drive_mailbox(now)
        } else {
            self.set_startup_stage(StartupStage::UploadMain(0));
            self.drive_startup(now)
        }
    }

    pub(super) fn drive_main_upload(&mut self, offset: usize, now: MonotonicTime) -> AicAction {
        let (main, _) = images(self.chip).expect("constructor validated the firmware image");
        if offset >= main.len() {
            self.set_startup_stage(StartupStage::UploadPatch(0));
            return self.drive_startup(now);
        }
        let end = (offset + UPLOAD_CHUNK).min(main.len());
        let payload = memory_block_write_payload(
            MAIN_ADDRESS.wrapping_add(offset as u32),
            &main[offset..end],
        );
        self.begin_debug_mailbox(DBG_MEM_BLOCK_WRITE_REQ, &payload, now);
        self.drive_mailbox(now)
    }

    pub(super) fn drive_patch_upload(&mut self, offset: usize, now: MonotonicTime) -> AicAction {
        let (_, patch) = images(self.chip).expect("constructor validated the firmware image");
        if offset >= patch.len() {
            self.set_startup_stage(if self.chip == ChipVariant::Aic8801 {
                StartupStage::ReadConfigBase
            } else {
                StartupStage::StartApplication
            });
            return self.drive_startup(now);
        }
        let end = (offset + UPLOAD_CHUNK).min(patch.len());
        let payload = memory_block_write_payload(
            PATCH_ADDRESS.wrapping_add(offset as u32),
            &patch[offset..end],
        );
        self.begin_debug_mailbox(DBG_MEM_BLOCK_WRITE_REQ, &payload, now);
        self.drive_mailbox(now)
    }

    pub(super) fn drive_patch_metadata(&mut self, index: usize, now: MonotonicTime) -> AicAction {
        let Some((address, value)) = self.patch_metadata(index) else {
            self.set_startup_stage(StartupStage::MaskedConfig(0));
            return self.drive_startup(now);
        };
        self.begin_debug_mailbox(
            DBG_MEM_WRITE_REQ,
            &memory_write_payload(address, value),
            now,
        );
        self.drive_mailbox(now)
    }

    pub(super) fn drive_masked_config(&mut self, index: usize, now: MonotonicTime) -> AicAction {
        if let Some(&(address, mask, value)) = MASKED_SYSTEM_CONFIG.get(index) {
            self.begin_debug_mailbox(
                DBG_MEM_MASK_WRITE_REQ,
                &memory_mask_write_payload(address, mask, value),
                now,
            );
            self.drive_mailbox(now)
        } else {
            self.set_startup_stage(StartupStage::SlowClock);
            self.drive_startup(now)
        }
    }

    pub(super) fn drive_config_base_read(&mut self, now: MonotonicTime) -> AicAction {
        self.begin_debug_mailbox(
            DBG_MEM_READ_REQ,
            &memory_read_payload(MAIN_ADDRESS + CONFIG_BASE_OFFSET),
            now,
        );
        self.drive_mailbox(now)
    }

    pub(in crate::device) fn complete_startup_mailbox(
        &mut self,
        result: Vec<u8>,
    ) -> Result<(), AicError> {
        let startup = self
            .lifecycle
            .startup
            .as_mut()
            .ok_or(AicError::CompletionMismatch)?;
        startup.stage = match startup.stage {
            StartupStage::ReadRevision => {
                if result.len() < 8 {
                    return Err(AicError::MalformedResponse);
                }
                let raw = u32::from_le_bytes([result[4], result[5], result[6], result[7]]);
                let revision = (raw & CHIP_REV_MASK) as u8;
                if !matches!(revision, 1 | 3 | 7) {
                    return Err(AicError::UnsupportedRevision(revision));
                }
                startup.revision = Some(revision);
                if self.chip == ChipVariant::Aic8801 {
                    StartupStage::SystemConfig(0)
                } else {
                    StartupStage::UploadMain(0)
                }
            }
            StartupStage::SystemConfig(index) => StartupStage::SystemConfig(index + 1),
            StartupStage::UploadMain(offset) => {
                let (main, _) =
                    images(self.chip).expect("constructor validated the firmware image");
                StartupStage::UploadMain((offset + UPLOAD_CHUNK).min(main.len()))
            }
            StartupStage::UploadPatch(offset) => {
                let (_, patch) =
                    images(self.chip).expect("constructor validated the firmware image");
                StartupStage::UploadPatch((offset + UPLOAD_CHUNK).min(patch.len()))
            }
            StartupStage::ReadConfigBase => {
                if result.len() < 8 {
                    return Err(AicError::MalformedResponse);
                }
                startup.config_base = Some(u32::from_le_bytes([
                    result[4], result[5], result[6], result[7],
                ]));
                StartupStage::PatchMetadata(0)
            }
            StartupStage::PatchMetadata(index) => StartupStage::PatchMetadata(index + 1),
            StartupStage::MaskedConfig(index) => StartupStage::MaskedConfig(index + 1),
            StartupStage::StartApplication => {
                if self.chip == ChipVariant::Aic8801 {
                    StartupStage::FastClock
                } else {
                    self.lifecycle.retry_at = Some(self.lifecycle.last_time.after(START_STABILIZE));
                    StartupStage::Stabilize
                }
            }
            StartupStage::StackStart => StartupStage::ArmChipInterrupt,
            _ => return Err(AicError::CompletionMismatch),
        };
        Ok(())
    }

    fn patch_metadata(&self, index: usize) -> Option<(u32, u32)> {
        match index {
            0 => Some((PATCH_ADDRESS_REGISTER, PATCH_TABLE_ADDRESS)),
            1 => Some((PATCH_COUNT_REGISTER, (PATCH_TABLE.len() * 2) as u32)),
            _ => {
                let pair = (index - 2) / 2;
                let field = (index - 2) % 2;
                let entry = PATCH_TABLE.get(pair)?;
                let config_base = self.lifecycle.startup.as_ref()?.config_base?;
                Some((
                    PATCH_TABLE_ADDRESS + (index as u32 - 2) * 4,
                    if field == 0 {
                        entry[0].wrapping_add(config_base)
                    } else {
                        entry[1]
                    },
                ))
            }
        }
    }
}
