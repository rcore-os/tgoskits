use core::{num::NonZeroU16, time::Duration};

use super::*;
use crate::{
    common::{CHIP_REV_ADDR, SDIOWIFI_FUNC_BLOCKSIZE},
    firmware::MAIN_ADDRESS,
    protocol::{
        DBG_MEM_READ_REQ, DBG_START_APP_REQ, MM_SET_STACK_START_REQ, TASK_MM, memory_read_payload,
        start_app_payload,
    },
    registers::{INTERRUPTS_ENABLED, interface_ready},
};

mod firmware;
mod vendor;

const START_STABILIZE: Duration = Duration::from_millis(200);
const FUNCTION_READY_DELAY_V2: Duration = Duration::from_millis(10);
const FUNCTION_READY_DELAY_V3: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StartupStage {
    EnableFunction,
    SetBlockSize,
    EnableFunctionInterrupt,
    VendorSetup(u8),
    VendorDelay,
    VendorReady,
    ReadRevision,
    SystemConfig(usize),
    UploadMain(usize),
    UploadPatch(usize),
    ReadConfigBase,
    PatchMetadata(usize),
    MaskedConfig(usize),
    SlowClock,
    StartApplication,
    FastClock,
    Stabilize,
    Reinitialize(u8),
    StackStart,
    ArmChipInterrupt,
    Complete,
}

pub(super) struct StartupState {
    pub stage: StartupStage,
    pub revision: Option<u8>,
    pub config_base: Option<u32>,
}

impl StartupState {
    pub(super) const fn new() -> Self {
        Self {
            stage: StartupStage::EnableFunction,
            revision: None,
            config_base: None,
        }
    }
}

impl AicDevice {
    pub(super) fn drive_startup(&mut self, now: MonotonicTime) -> AicAction {
        if self.lifecycle.mailbox.is_some() {
            return self.drive_mailbox(now);
        }
        let stage = self
            .lifecycle
            .startup
            .as_ref()
            .map(|startup| startup.stage)
            .unwrap_or(StartupStage::Complete);
        match stage {
            StartupStage::EnableFunction => self.emit(
                IoPurpose::Startup,
                SdioRequestKind::EnableFunction(function(1)),
            ),
            StartupStage::SetBlockSize => self.emit(
                IoPurpose::Startup,
                SdioRequestKind::SetBlockSize {
                    function: function(1),
                    block_size: NonZeroU16::new(SDIOWIFI_FUNC_BLOCKSIZE).unwrap(),
                },
            ),
            StartupStage::EnableFunctionInterrupt => self.emit(
                IoPurpose::Startup,
                SdioRequestKind::EnableFunctionInterrupt(function(1)),
            ),
            StartupStage::VendorSetup(index) => match self.vendor_setup_operation(index, false) {
                Some(kind) => self.emit(IoPurpose::Startup, kind),
                None => {
                    self.set_startup_stage(StartupStage::VendorDelay);
                    self.lifecycle.retry_at = Some(now.after(if self.chip.is_v3() {
                        FUNCTION_READY_DELAY_V3
                    } else {
                        FUNCTION_READY_DELAY_V2
                    }));
                    AicAction::RetryAt(
                        self.lifecycle
                            .retry_at
                            .expect("vendor delay was installed above"),
                    )
                }
            },
            StartupStage::VendorDelay => {
                self.set_startup_stage(if self.chip.is_v3() {
                    StartupStage::VendorReady
                } else {
                    StartupStage::ReadRevision
                });
                self.drive_startup(now)
            }
            StartupStage::VendorReady => self.emit(
                IoPurpose::Startup,
                read_byte(
                    1,
                    self.registers
                        .sleep_status
                        .expect("v3 chips define a sleep-status register"),
                ),
            ),
            StartupStage::ReadRevision => {
                self.begin_debug_mailbox(
                    DBG_MEM_READ_REQ,
                    &memory_read_payload(CHIP_REV_ADDR),
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::SystemConfig(index) => self.drive_system_config(index, now),
            StartupStage::UploadMain(offset) => self.drive_main_upload(offset, now),
            StartupStage::UploadPatch(offset) => self.drive_patch_upload(offset, now),
            StartupStage::ReadConfigBase => self.drive_config_base_read(now),
            StartupStage::PatchMetadata(index) => self.drive_patch_metadata(index, now),
            StartupStage::MaskedConfig(index) => self.drive_masked_config(index, now),
            StartupStage::SlowClock => {
                self.emit(IoPurpose::Startup, SdioRequestKind::SetClockHz(400_000))
            }
            StartupStage::StartApplication => {
                self.begin_debug_mailbox(
                    DBG_START_APP_REQ,
                    &start_app_payload(MAIN_ADDRESS, 1),
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::FastClock => {
                self.emit(IoPurpose::Startup, SdioRequestKind::SetClockHz(25_000_000))
            }
            StartupStage::Stabilize => {
                self.set_startup_stage(StartupStage::Reinitialize(0));
                self.drive_startup(now)
            }
            StartupStage::Reinitialize(index) => match self.vendor_setup_operation(index, true) {
                Some(kind) => self.emit(IoPurpose::Startup, kind),
                None => {
                    self.set_startup_stage(StartupStage::StackStart);
                    self.drive_startup(now)
                }
            },
            StartupStage::StackStart => {
                let vendor = if self.chip.is_v3() { 0x20 } else { 0 };
                self.begin_lmac_mailbox(
                    MM_SET_STACK_START_REQ,
                    TASK_MM,
                    &[1, 0, vendor, 0],
                    MM_SET_STACK_START_REQ + 1,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::ArmChipInterrupt => self.emit(
                IoPurpose::Startup,
                write_byte(1, self.registers.interrupt_enable, INTERRUPTS_ENABLED),
            ),
            StartupStage::Complete => {
                self.lifecycle.startup = None;
                self.lifecycle.state = AicState::Ready;
                self.data.events.push_back(AicEvent::Started {
                    mac_address: self.data.mac_address,
                });
                AicAction::Event(self.data.events.pop_front().unwrap())
            }
        }
    }

    pub(super) fn consume_startup_response(
        &mut self,
        response: SdioResponse,
        now: MonotonicTime,
    ) -> Result<(), AicError> {
        let stage = self
            .lifecycle
            .startup
            .as_ref()
            .map(|startup| startup.stage)
            .ok_or(AicError::CompletionMismatch)?;
        match stage {
            StartupStage::EnableFunction => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::SetBlockSize);
            }
            StartupStage::SetBlockSize => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::EnableFunctionInterrupt);
            }
            StartupStage::EnableFunctionInterrupt => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::VendorSetup(0));
            }
            StartupStage::VendorSetup(index) => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::VendorSetup(index + 1));
            }
            StartupStage::VendorReady => {
                let value = expect_byte(response)?;
                if !interface_ready(value) {
                    return Err(AicError::Sdio(SdioFailure::Timeout));
                }
                self.set_startup_stage(StartupStage::ReadRevision);
            }
            StartupStage::SlowClock => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::StartApplication);
            }
            StartupStage::FastClock => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::Stabilize);
                self.lifecycle.retry_at = Some(now.after(START_STABILIZE));
            }
            StartupStage::Reinitialize(index) => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::Reinitialize(index + 1));
            }
            StartupStage::ArmChipInterrupt => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::Complete);
            }
            _ => return Err(AicError::CompletionMismatch),
        }
        Ok(())
    }

    fn set_startup_stage(&mut self, stage: StartupStage) {
        if let Some(startup) = self.lifecycle.startup.as_mut() {
            startup.stage = stage;
        }
    }
}
