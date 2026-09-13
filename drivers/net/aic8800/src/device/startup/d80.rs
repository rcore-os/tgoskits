//! D80 image patch metadata, following the Sipeed BSP patch configuration ABI.

use alloc::vec::Vec;

use super::StartupStage;
use crate::{
    device::*,
    firmware::D80_MAIN_ADDRESS,
    protocol::{
        DBG_MEM_READ_REQ, DBG_MEM_WRITE_REQ, debug_memory_read, memory_read_payload,
        memory_write_payload, require_debug_memory_write,
    },
};

const CONFIG_POINTER: u32 = D80_MAIN_ADDRESS + 0x198;
const STRUCT_POINTER: u32 = CONFIG_POINTER + 8;
const BUFFER_POINTER: u32 = CONFIG_POINTER + 12;
const VERSION_ADDRESS: u32 = D80_MAIN_ADDRESS + 0x1c;
const RELOCATED_BUFFER_VERSION: u32 = 0x0609_0100;
const LEGACY_BUFFER: u32 = 0x0016_f800;
const PATCH_PAIRS: [[u32; 2]; 3] = [
    [0x00b4, 0xf301_0000], // 2.4 GHz only.
    [0x0170, 0x0100_000a], // Receive aggregation.
    [0x0188, 0x0000_0003], // Power calibration and channel TX power limits.
];
const PATCH_WRITES: u8 = 4 + PATCH_PAIRS.len() as u8 * 2 + 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PatchLayout {
    config: u32,
    structure: u32,
    buffer: u32,
}

impl PatchLayout {
    fn new(config: u32, structure: u32, buffer: u32) -> Result<Self, AicError> {
        // Firmware-provided pointers must address complete aligned objects.
        // Reject corruption before emitting any patch writes.
        for (address, last_offset) in [(config, 0x188), (structure, 60), (buffer, 20)] {
            if address == 0
                || !address.is_multiple_of(4)
                || address.checked_add(last_offset).is_none()
            {
                return Err(AicError::MalformedResponse);
            }
        }
        Ok(Self {
            config,
            structure,
            buffer,
        })
    }

    fn write(self, index: u8) -> Result<(u32, u32), AicError> {
        Ok(match index {
            0 => (self.structure, 0x4843_5450),
            1 => (self.structure + 8, 0x5054_4348),
            2 => (self.structure + 4, self.buffer),
            3 => (self.structure + 12, PATCH_PAIRS.len() as u32),
            4..=9 => {
                let word = usize::from(index - 4);
                let [offset, value] = PATCH_PAIRS[word / 2];
                (
                    self.buffer + word as u32 * 4,
                    if word.is_multiple_of(2) {
                        self.config + offset
                    } else {
                        value
                    },
                )
            }
            10..=13 => (self.structure + 48 + u32::from(index - 10) * 4, 0),
            _ => return Err(AicError::CompletionMismatch),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum D80PatchStage {
    ReadConfigBase,
    ReadStructBase { config: u32 },
    ReadVersion { config: u32, structure: u32 },
    ReadPatchBuffer { config: u32, structure: u32 },
    Write { layout: PatchLayout, index: u8 },
}

impl D80PatchStage {
    fn read_address(self) -> Option<u32> {
        match self {
            Self::ReadConfigBase => Some(CONFIG_POINTER),
            Self::ReadStructBase { .. } => Some(STRUCT_POINTER),
            Self::ReadVersion { .. } => Some(VERSION_ADDRESS),
            Self::ReadPatchBuffer { .. } => Some(BUFFER_POINTER),
            Self::Write { .. } => None,
        }
    }

    fn request(self) -> Result<(u16, Vec<u8>), AicError> {
        if let Some(address) = self.read_address() {
            return Ok((DBG_MEM_READ_REQ, memory_read_payload(address).to_vec()));
        }
        let Self::Write { layout, index } = self else {
            return Err(AicError::CompletionMismatch);
        };
        let (address, value) = layout.write(index)?;
        Ok((
            DBG_MEM_WRITE_REQ,
            memory_write_payload(address, value).to_vec(),
        ))
    }

    fn complete(self, result: &[u8]) -> Result<Option<Self>, AicError> {
        let next = if let Some(address) = self.read_address() {
            let value = debug_memory_read(result, address)?;
            match self {
                Self::ReadConfigBase => Self::ReadStructBase { config: value },
                Self::ReadStructBase { config } => Self::ReadVersion {
                    config,
                    structure: value,
                },
                Self::ReadVersion { config, structure } => {
                    if value > RELOCATED_BUFFER_VERSION {
                        Self::ReadPatchBuffer { config, structure }
                    } else {
                        Self::Write {
                            layout: PatchLayout::new(config, structure, LEGACY_BUFFER)?,
                            index: 0,
                        }
                    }
                }
                Self::ReadPatchBuffer { config, structure } => Self::Write {
                    layout: PatchLayout::new(config, structure, value)?,
                    index: 0,
                },
                Self::Write { .. } => return Err(AicError::CompletionMismatch),
            }
        } else {
            let Self::Write { layout, index } = self else {
                return Err(AicError::CompletionMismatch);
            };
            let (address, value) = layout.write(index)?;
            require_debug_memory_write(result, address, Some(value))?;
            if index + 1 == PATCH_WRITES {
                return Ok(None);
            }
            Self::Write {
                layout,
                index: index + 1,
            }
        };
        Ok(Some(next))
    }
}

impl AicDevice {
    pub(super) fn drive_d80_patch(
        &mut self,
        stage: D80PatchStage,
        now: MonotonicTime,
    ) -> AicAction {
        let (message, payload) = match stage.request() {
            Ok(request) => request,
            Err(error) => return self.fail(error),
        };
        self.begin_debug_mailbox(message, &payload, now);
        self.drive_mailbox(now)
    }

    pub(super) fn complete_d80_patch_mailbox(
        &mut self,
        stage: D80PatchStage,
        result: Vec<u8>,
    ) -> Result<StartupStage, AicError> {
        Ok(match stage.complete(&result)? {
            Some(next) => StartupStage::D80Patch(next),
            None => StartupStage::StartApplication,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirmation(address: u32, value: u32) -> Vec<u8> {
        [address.to_le_bytes(), value.to_le_bytes()].concat()
    }

    #[test]
    fn firmware_metadata_drives_the_complete_patch_transaction() {
        for (version, buffer) in [(0x0609_0100, 0x0016_f800), (0x0609_0101, 0x0017_0000)] {
            let mut stage = D80PatchStage::ReadConfigBase;
            for (address, value) in [
                (D80_MAIN_ADDRESS + 0x198, 0x0002_0000),
                (D80_MAIN_ADDRESS + 0x1a0, 0x0016_e000),
                (D80_MAIN_ADDRESS + 0x1c, version),
            ]
            .into_iter()
            .chain((version > 0x0609_0100).then_some((D80_MAIN_ADDRESS + 0x1a4, buffer)))
            {
                assert_eq!(
                    stage.request().unwrap(),
                    (DBG_MEM_READ_REQ, address.to_le_bytes().to_vec())
                );
                stage = stage
                    .complete(&confirmation(address, value))
                    .unwrap()
                    .unwrap();
            }
            // Vendor aic_patch_t header, address/value pairs, then zero block sizes.
            let expected = [
                (0x0016_e000, 0x4843_5450),
                (0x0016_e008, 0x5054_4348),
                (0x0016_e004, buffer),
                (0x0016_e00c, 3),
                (buffer, 0x0002_00b4),
                (buffer + 4, 0xf301_0000),
                (buffer + 8, 0x0002_0170),
                (buffer + 12, 0x0100_000a),
                (buffer + 16, 0x0002_0188),
                (buffer + 20, 3),
                (0x0016_e030, 0),
                (0x0016_e034, 0),
                (0x0016_e038, 0),
                (0x0016_e03c, 0),
            ];
            for (index, (address, value)) in expected.into_iter().enumerate() {
                assert_eq!(
                    stage.request().unwrap(),
                    (DBG_MEM_WRITE_REQ, confirmation(address, value))
                );
                assert_eq!(
                    stage.complete(&confirmation(address, value ^ 1)),
                    Err(AicError::MalformedResponse)
                );
                let next = stage.complete(&confirmation(address, value)).unwrap();
                if index + 1 == expected.len() {
                    assert_eq!(next, None);
                } else {
                    stage = next.unwrap();
                }
            }
        }
    }

    #[test]
    fn corrupt_patch_metadata_is_rejected_before_any_write() {
        for buffer in [0, 3, 0xffff_fff0] {
            let stage = D80PatchStage::ReadPatchBuffer {
                config: 0x20000,
                structure: 0x16e000,
            };
            assert_eq!(
                stage.complete(&confirmation(BUFFER_POINTER, buffer)),
                Err(AicError::MalformedResponse)
            );
        }
    }
}
