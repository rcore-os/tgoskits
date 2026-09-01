use core::{num::NonZeroU16, time::Duration};

use super::*;
use crate::{
    common::{CHIP_REV_ADDR, SDIOWIFI_FUNC_BLOCKSIZE},
    firmware::{D80_MAIN_ADDRESS, DC_BOOT_ADDRESS},
    lmac::{
        ME_CHAN_CONFIG_CFM, ME_CHAN_CONFIG_REQ, ME_CONFIG_CFM, ME_CONFIG_REQ, MM_ADD_IF_CFM,
        MM_ADD_IF_REQ, MM_GET_MAC_ADDR_CFM, MM_GET_MAC_ADDR_REQ, MM_RESET_CFM, MM_RESET_REQ,
        MM_SET_FILTER_CFM, MM_SET_FILTER_REQ, MM_SET_RF_CALIB_CFM, MM_SET_RF_CALIB_REQ,
        MM_SET_RF_CONFIG_CFM, MM_SET_RF_CONFIG_REQ, MM_SET_STACK_START_CFM, MM_SET_STACK_START_REQ,
        MM_SET_TXPWR_IDX_LVL_CFM, MM_SET_TXPWR_IDX_LVL_REQ, MM_START_CFM, MM_START_REQ,
        RfCalibrationBand, TASK_ME, TASK_MM, add_interface_payload, channel_config_payload,
        filter_payload, get_mac_payload, me_config_payload, rf_calibration_payload,
        stack_start_payload, start_payload, tx_power_level_payload,
    },
    profile::FirmwareProfile,
    protocol::{
        DBG_MEM_READ_REQ, DBG_START_APP_REQ, DebugConfirmationError, memory_read_payload,
        start_app_payload,
    },
    registers::{INTERRUPTS_ENABLED, interface_ready},
};

mod dc;
mod firmware;
mod vendor;

use dc::{DcStage, DcStartupState};

const START_STABILIZE: Duration = Duration::from_millis(200);
const FUNCTION_READY_DELAY_V2: Duration = Duration::from_millis(10);
const FUNCTION_READY_DELAY_V3: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FunctionSetupStep {
    SetBlockSize,
    Enable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupStage {
    ConfigureFunction { index: u8, step: FunctionSetupStep },
    EnableFunctionInterrupt { index: u8 },
    VendorSetup(u8),
    VendorDelay,
    VendorReady,
    ReadRevision,
    UploadMain(usize),
    Dc(DcStage),
    StartApplication,
    Stabilize,
    Reinitialize(u8),
    StackStart,
    TxPowerLevel,
    RfConfig(u8),
    RfCalibration,
    ReadMacAddress,
    FirmwareReset,
    ConfigureMac,
    ConfigureChannels,
    AddStationInterface,
    StartMac,
    SetFilter,
    ArmChipInterrupt,
    Complete,
}

pub(super) struct StartupState {
    stage: StartupStage,
    revision: Option<u8>,
    dc: Option<DcStartupState>,
}

impl StartupState {
    pub(super) const fn new() -> Self {
        Self {
            stage: StartupStage::ConfigureFunction {
                index: 0,
                step: FunctionSetupStep::SetBlockSize,
            },
            revision: None,
            dc: None,
        }
    }
}

impl AicDevice {
    pub(super) fn startup_stage_diagnostic(&self) -> Option<alloc::string::String> {
        self.lifecycle
            .startup
            .as_ref()
            .map(|startup| alloc::format!("{:?}", startup.stage))
    }

    pub(super) fn log_startup_confirmation_error(
        &self,
        result_length: usize,
        result_header: &[u8],
        error: &AicError,
    ) {
        let stage = self.lifecycle.startup.as_ref().map(|startup| startup.stage);
        log::error!(
            "[wifi] AIC startup confirmation rejected: stage={stage:?} result_len={result_length} \
             result={result_header:02x?} error={error}"
        );
    }

    pub(super) fn drive_startup(&mut self, now: MonotonicTime) -> AicAction {
        if self.mailbox_timed_out(now) {
            return self.drive_mailbox(now);
        }
        if self.lifecycle.mailbox.is_some() {
            if self.mailbox_waiting_for_receive()
                && let Some(action) = self.drive_receive_scan()
            {
                return action;
            }
            return self.drive_mailbox(now);
        }
        let stage = self
            .lifecycle
            .startup
            .as_ref()
            .map(|startup| startup.stage)
            .unwrap_or(StartupStage::Complete);
        match stage {
            StartupStage::ConfigureFunction { index, step } => {
                let Some(number) = self.startup_function(usize::from(index)) else {
                    self.set_startup_stage(StartupStage::EnableFunctionInterrupt { index: 0 });
                    return self.drive_startup(now);
                };
                match step {
                    FunctionSetupStep::SetBlockSize => self.emit(
                        IoPurpose::Startup,
                        SdioRequestKind::SetBlockSize {
                            function: function(number),
                            block_size: NonZeroU16::new(SDIOWIFI_FUNC_BLOCKSIZE).unwrap(),
                        },
                    ),
                    FunctionSetupStep::Enable => self.emit(
                        IoPurpose::Startup,
                        SdioRequestKind::EnableFunction(function(number)),
                    ),
                }
            }
            StartupStage::EnableFunctionInterrupt { index } => {
                let Some(number) = self.startup_function(usize::from(index)) else {
                    self.set_startup_stage(StartupStage::VendorSetup(0));
                    return self.drive_startup(now);
                };
                self.emit(
                    IoPurpose::Startup,
                    SdioRequestKind::EnableFunctionInterrupt(function(number)),
                )
            }
            StartupStage::VendorSetup(index) => match self.vendor_setup_operation(index, false) {
                Some(kind) => self.emit(IoPurpose::Startup, kind),
                None => {
                    self.set_startup_stage(StartupStage::VendorDelay);
                    self.lifecycle.retry_at = Some(now.after(
                        if self.transport_generation() == crate::profile::TransportGeneration::V3 {
                            FUNCTION_READY_DELAY_V3
                        } else {
                            FUNCTION_READY_DELAY_V2
                        },
                    ));
                    AicAction::RetryAt(
                        self.lifecycle
                            .retry_at
                            .expect("vendor delay was installed above"),
                    )
                }
            },
            StartupStage::VendorDelay => {
                self.set_startup_stage(
                    if self.transport_generation() == crate::profile::TransportGeneration::V3 {
                        StartupStage::VendorReady
                    } else {
                        StartupStage::ReadRevision
                    },
                );
                self.drive_startup(now)
            }
            StartupStage::VendorReady => self.emit(
                IoPurpose::Startup,
                read_byte(
                    1,
                    self.registers()
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
            StartupStage::UploadMain(offset) => self.drive_main_upload(offset, now),
            StartupStage::Dc(stage) => self.drive_dc_startup(stage, now),
            StartupStage::StartApplication => {
                let (address, boot_type) = match self.firmware_profile() {
                    FirmwareProfile::Aic8800Dc => (DC_BOOT_ADDRESS, 5),
                    FirmwareProfile::Aic8800D80 => (D80_MAIN_ADDRESS, 1),
                };
                self.begin_debug_mailbox(
                    DBG_START_APP_REQ,
                    &start_app_payload(address, boot_type),
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::Stabilize => {
                self.set_startup_stage(StartupStage::Reinitialize(0));
                self.drive_startup(now)
            }
            StartupStage::Reinitialize(index) => match self.vendor_setup_operation(index, true) {
                Some(kind) => self.emit(IoPurpose::Startup, kind),
                None => {
                    self.set_startup_stage(
                        if self.firmware_profile() == FirmwareProfile::Aic8800Dc
                            && self.dc_has_applicable_dpd_result()
                        {
                            StartupStage::Dc(DcStage::ReadRuntimeMiscRamAddress)
                        } else {
                            StartupStage::StackStart
                        },
                    );
                    self.drive_startup(now)
                }
            },
            StartupStage::StackStart => {
                let vendor =
                    if self.transport_generation() == crate::profile::TransportGeneration::V3 {
                        0x20
                    } else {
                        0
                    };
                self.begin_lmac_mailbox(
                    MM_SET_STACK_START_REQ,
                    TASK_MM,
                    &stack_start_payload(vendor),
                    MM_SET_STACK_START_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::TxPowerLevel => {
                self.begin_lmac_mailbox(
                    MM_SET_TXPWR_IDX_LVL_REQ,
                    TASK_MM,
                    &tx_power_level_payload(),
                    MM_SET_TXPWR_IDX_LVL_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::RfConfig(index) => {
                let payload = match self.dc_lmac_rf_payload(index) {
                    Ok(Some(payload)) => payload,
                    Ok(None) => return self.fail(AicError::CompletionMismatch),
                    Err(error) => return self.fail(error),
                };
                self.begin_lmac_mailbox(
                    MM_SET_RF_CONFIG_REQ,
                    TASK_MM,
                    &payload,
                    MM_SET_RF_CONFIG_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::RfCalibration => {
                let band = match self.firmware_profile() {
                    FirmwareProfile::Aic8800Dc => RfCalibrationBand::Ghz2Only,
                    FirmwareProfile::Aic8800D80 => RfCalibrationBand::DualBand,
                };
                self.begin_lmac_mailbox(
                    MM_SET_RF_CALIB_REQ,
                    TASK_MM,
                    &rf_calibration_payload(band),
                    MM_SET_RF_CALIB_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::ReadMacAddress => {
                self.begin_lmac_mailbox(
                    MM_GET_MAC_ADDR_REQ,
                    TASK_MM,
                    &get_mac_payload(),
                    MM_GET_MAC_ADDR_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::FirmwareReset => {
                self.begin_lmac_mailbox(MM_RESET_REQ, TASK_MM, &[], MM_RESET_CFM, now);
                self.drive_mailbox(now)
            }
            StartupStage::ConfigureMac => {
                self.begin_lmac_mailbox(
                    ME_CONFIG_REQ,
                    TASK_ME,
                    &me_config_payload(),
                    ME_CONFIG_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::ConfigureChannels => {
                self.begin_lmac_mailbox(
                    ME_CHAN_CONFIG_REQ,
                    TASK_ME,
                    &channel_config_payload(),
                    ME_CHAN_CONFIG_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::AddStationInterface => {
                let Some(mac) = self.data.link.mac_address() else {
                    return self.fail(AicError::InvalidMacAddress);
                };
                self.begin_lmac_mailbox(
                    MM_ADD_IF_REQ,
                    TASK_MM,
                    &add_interface_payload(mac, 0),
                    MM_ADD_IF_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::StartMac => {
                self.begin_lmac_mailbox(MM_START_REQ, TASK_MM, &start_payload(), MM_START_CFM, now);
                self.drive_mailbox(now)
            }
            StartupStage::SetFilter => {
                self.begin_lmac_mailbox(
                    MM_SET_FILTER_REQ,
                    TASK_MM,
                    &filter_payload(),
                    MM_SET_FILTER_CFM,
                    now,
                );
                self.drive_mailbox(now)
            }
            StartupStage::ArmChipInterrupt => self.emit(
                IoPurpose::Startup,
                write_byte(
                    self.data_function(),
                    self.registers().interrupt_enable,
                    INTERRUPTS_ENABLED,
                ),
            ),
            StartupStage::Complete => {
                let Some(mac_address) = self.data.link.mac_address() else {
                    return self.fail(AicError::InvalidMacAddress);
                };
                if self.data.link.interface_index().is_none() {
                    return self.fail(AicError::MalformedResponse);
                }
                self.lifecycle.startup = None;
                self.lifecycle.state = AicState::Ready;
                self.data
                    .events
                    .push_back(AicEvent::Started { mac_address });
                AicAction::Event(self.data.events.pop_front().unwrap())
            }
        }
    }

    pub(super) fn consume_startup_response(
        &mut self,
        response: SdioResponse,
        _now: MonotonicTime,
    ) -> Result<(), AicError> {
        let stage = self
            .lifecycle
            .startup
            .as_ref()
            .map(|startup| startup.stage)
            .ok_or(AicError::CompletionMismatch)?;
        match stage {
            StartupStage::ConfigureFunction { index, step } => {
                expect_unit(response)?;
                self.set_startup_stage(match step {
                    FunctionSetupStep::SetBlockSize => StartupStage::ConfigureFunction {
                        index,
                        step: FunctionSetupStep::Enable,
                    },
                    FunctionSetupStep::Enable => StartupStage::ConfigureFunction {
                        index: index.saturating_add(1),
                        step: FunctionSetupStep::SetBlockSize,
                    },
                });
            }
            StartupStage::EnableFunctionInterrupt { index } => {
                expect_unit(response)?;
                self.set_startup_stage(StartupStage::EnableFunctionInterrupt {
                    index: index.saturating_add(1),
                });
            }
            StartupStage::VendorSetup(index) => {
                self.validate_vendor_setup_readback(index, false, response)?;
                self.set_startup_stage(StartupStage::VendorSetup(index + 1));
            }
            StartupStage::VendorReady => {
                let value = expect_byte(response)?;
                if !interface_ready(value) {
                    return Err(AicError::Sdio(SdioFailure::Timeout));
                }
                self.set_startup_stage(StartupStage::ReadRevision);
            }
            StartupStage::Reinitialize(index) => {
                self.validate_vendor_setup_readback(index, true, response)?;
                self.set_startup_stage(StartupStage::Reinitialize(index + 1));
            }
            StartupStage::ArmChipInterrupt => {
                expect_write_readback(response, INTERRUPTS_ENABLED)?;
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

fn map_debug_error(message_id: u16, error: DebugConfirmationError) -> AicError {
    match error {
        DebugConfirmationError::Malformed => AicError::MalformedResponse,
        DebugConfirmationError::Rejected(status) => {
            AicError::DebugFirmwareRejected { message_id, status }
        }
    }
}

impl From<DebugConfirmationError> for AicError {
    fn from(error: DebugConfirmationError) -> Self {
        map_debug_error(0, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ChipVariant;

    #[test]
    fn startup_refuses_to_publish_an_all_zero_mac_address() {
        let mut device = AicDevice::new(ChipVariant::Aic8800D80).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(StartupState {
            stage: StartupStage::Complete,
            revision: Some(3),
            dc: None,
        });

        assert_eq!(
            device.drive_startup(MonotonicTime::from_nanos(0)),
            AicAction::Event(AicEvent::Failed(AicError::InvalidMacAddress))
        );
    }

    #[test]
    fn vendor_cmd52_write_accepts_the_raw_readback_byte() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(StartupState {
            stage: StartupStage::VendorSetup(0),
            revision: None,
            dc: None,
        });

        assert_eq!(
            device.consume_startup_response(SdioResponse::Byte(1), MonotonicTime::from_nanos(0)),
            Ok(())
        );
        assert!(matches!(
            device.lifecycle.startup.as_ref().map(|state| state.stage),
            Some(StartupStage::VendorSetup(1))
        ));
    }

    #[test]
    fn dc_rf_calibration_request_does_not_enable_5ghz_calibration() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(StartupState {
            stage: StartupStage::RfCalibration,
            revision: Some(7),
            dc: None,
        });

        let AicAction::SubmitSdio(request) = device.drive_startup(MonotonicTime::from_nanos(0))
        else {
            panic!("expected the DC RF calibration mailbox write")
        };
        let SdioRequestKind::Write { bytes, .. } = request.kind else {
            panic!("expected an SDIO FIFO write")
        };

        assert_eq!(&bytes[8..10], &MM_SET_RF_CALIB_REQ.to_le_bytes());
        assert_eq!(&bytes[20..24], &0u32.to_le_bytes());
    }

    #[test]
    fn dc_arms_both_device_interrupt_registers_before_firmware_startup() {
        let device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();

        for (index, expected_function) in [(4, 1), (5, 2)] {
            assert!(matches!(
                device.vendor_setup_operation(index, false),
                Some(SdioRequestKind::WriteByte {
                    function,
                    address,
                    value: INTERRUPTS_ENABLED,
                    ..
                }) if function.get() == expected_function
                    && address.get() == device.registers().interrupt_enable
            ));
        }
    }

    #[test]
    fn startup_mailbox_is_not_preempted_by_a_stale_receive_scan() {
        let mut device = AicDevice::new(ChipVariant::Aic8800DC).unwrap();
        device.lifecycle.state = AicState::Starting;
        device.lifecycle.startup = Some(StartupState {
            stage: StartupStage::ReadRevision,
            revision: None,
            dc: None,
        });
        device.request_receive_scan();

        let action = device.drive_startup(MonotonicTime::from_nanos(0));
        assert!(matches!(
            action,
            AicAction::SubmitSdio(SdioRequest {
                kind: SdioRequestKind::Write { function, .. },
                ..
            }) if function.get() == 2
        ));
    }
}
