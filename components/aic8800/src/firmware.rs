//! Pure AIC firmware bring-up protocol.
//!
//! This module owns immutable firmware data and a bounded request/confirmation
//! state machine. It deliberately has no SDIO host, IRQ, task, timer, or wake
//! dependency: the maintenance owner submits each returned request and feeds
//! the exact confirmation back after an acknowledged host interrupt.

mod dc;
mod dc_rf_cfg;

use alloc::{vec, vec::Vec};

use crate::{
    common::{ChipVariant, HOST_START_APP_AUTO, TASK_DBG},
    softap::LmacRequest,
};

macro_rules! firmware_path {
    ($name:literal) => {
        concat!(env!("OUT_DIR"), "/firmware/", $name)
    };
}

const AIC8801: &[u8] = include_bytes!(firmware_path!("fmacfw.bin"));
const AIC8800_DC: &[u8] = include_bytes!(firmware_path!("fmacfw_patch_8800dc_u02.bin"));
const AIC8800_DC_H: &[u8] = include_bytes!(firmware_path!("fmacfw_patch_8800dc_h_u02.bin"));
const AIC8800_DC_H_CALIB: &[u8] = include_bytes!(firmware_path!("fmacfw_calib_8800dc_h_u02.bin"));
const AIC8800_DC_PATCH_TABLE: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_tbl_8800dc_u02.bin"));
const AIC8800_DC_H_PATCH_TABLE: &[u8] =
    include_bytes!(firmware_path!("fmacfw_patch_tbl_8800dc_h_u02.bin"));
const AIC8800_D80: &[u8] = include_bytes!(firmware_path!("fmacfw_8800d80_u02.bin"));

pub(crate) const RAM_FMAC_FW_ADDR: u32 = 0x0012_0000;
pub(crate) const ROM_FMAC_PATCH_ADDR: u32 = 0x0018_0000;
pub(crate) const ROM_FMAC_CALIB_ADDR: u32 = 0x0013_0000;
pub(crate) const UPLOAD_CHUNK_BYTES: usize = 1024;
const DBG_MEM_READ_REQ: u16 = 0x0400;
const DBG_MEM_WRITE_REQ: u16 = 0x0402;
const DBG_MEM_BLOCK_WRITE_REQ: u16 = 0x040b;
const DBG_START_APP_REQ: u16 = 0x040d;
const DBG_MEM_MASK_WRITE_REQ: u16 = 0x0411;

#[derive(Clone, Copy)]
pub(crate) struct FirmwareImage {
    destination: u32,
    bytes: &'static [u8],
    boot_address: u32,
    boot_type: u32,
}

#[derive(Clone, Copy)]
pub(crate) enum FirmwarePlan {
    Simple(FirmwareImage),
    Dc,
}

pub(crate) fn plan(chip: ChipVariant) -> Option<FirmwarePlan> {
    let image = match chip {
        ChipVariant::Aic8801 => FirmwareImage {
            destination: RAM_FMAC_FW_ADDR,
            bytes: AIC8801,
            boot_address: RAM_FMAC_FW_ADDR,
            boot_type: HOST_START_APP_AUTO,
        },
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW => return Some(FirmwarePlan::Dc),
        ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2 => FirmwareImage {
            destination: RAM_FMAC_FW_ADDR,
            bytes: AIC8800_D80,
            boot_address: RAM_FMAC_FW_ADDR,
            boot_type: HOST_START_APP_AUTO,
        },
        ChipVariant::Unknown => return None,
    };
    Some(FirmwarePlan::Simple(image))
}

#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {
    #[error("firmware state machine received a confirmation without a pending request")]
    UnexpectedConfirmation,
    #[error("firmware request {operation} has a truncated confirmation")]
    TruncatedConfirmation { operation: &'static str },
    #[error(
        "firmware request {operation} confirmed address {actual:#010x}, expected {expected:#010x}"
    )]
    ConfirmationAddress {
        operation: &'static str,
        expected: u32,
        actual: u32,
    },
    #[error("firmware start request was rejected with status {status:#010x}")]
    StartRejected { status: u32 },
    #[error("unsupported AIC8800DC revision chip_id={chip_id:#04x} sub_id={sub_id:#04x}")]
    UnsupportedDcRevision { chip_id: u8, sub_id: u8 },
    #[error("invalid AIC8800DC patch table: {reason}")]
    InvalidPatchTable { reason: &'static str },
}

pub(crate) enum FirmwarePoll {
    Request(LmacRequest),
    Ready,
}

pub(crate) struct FirmwareMachine {
    kind: FirmwareKind,
    pending: Option<PendingOperation>,
}

enum FirmwareKind {
    Simple(SimpleFirmware),
    Dc(dc::DcFirmware),
}

impl FirmwareMachine {
    pub(crate) fn new(plan: FirmwarePlan) -> Self {
        let kind = match plan {
            FirmwarePlan::Simple(image) => FirmwareKind::Simple(SimpleFirmware::new(image)),
            FirmwarePlan::Dc => FirmwareKind::Dc(dc::DcFirmware::new()),
        };
        Self {
            kind,
            pending: None,
        }
    }

    /// Advances exactly one hardware request per call.
    pub(crate) fn poll(
        &mut self,
        confirmation: Option<&[u8]>,
    ) -> Result<FirmwarePoll, FirmwareError> {
        match (self.pending.take(), confirmation) {
            (Some(pending), Some(payload)) => {
                let completion = pending.parse(payload)?;
                match &mut self.kind {
                    FirmwareKind::Simple(machine) => machine.complete(completion)?,
                    FirmwareKind::Dc(machine) => machine.complete(completion)?,
                }
            }
            (Some(pending), None) => {
                self.pending = Some(pending);
                return Err(FirmwareError::TruncatedConfirmation {
                    operation: "missing confirmation",
                });
            }
            (None, Some(_)) => return Err(FirmwareError::UnexpectedConfirmation),
            (None, None) => {}
        }

        let operation = match &mut self.kind {
            FirmwareKind::Simple(machine) => machine.next_operation()?,
            FirmwareKind::Dc(machine) => machine.next_operation()?,
        };
        let Some(operation) = operation else {
            return Ok(FirmwarePoll::Ready);
        };
        let (request, pending) = operation.into_request();
        self.pending = Some(pending);
        Ok(FirmwarePoll::Request(request))
    }
}

struct SimpleFirmware {
    image: FirmwareImage,
    offset: usize,
    start_issued: bool,
}

impl SimpleFirmware {
    const fn new(image: FirmwareImage) -> Self {
        Self {
            image,
            offset: 0,
            start_issued: false,
        }
    }

    fn next_operation(&mut self) -> Result<Option<DebugOperation>, FirmwareError> {
        if self.offset < self.image.bytes.len() {
            let end = (self.offset + UPLOAD_CHUNK_BYTES).min(self.image.bytes.len());
            let operation = DebugOperation::block_write(
                self.image.destination.wrapping_add(self.offset as u32),
                &self.image.bytes[self.offset..end],
            );
            self.offset = end;
            return Ok(Some(operation));
        }
        if !self.start_issued {
            self.start_issued = true;
            return Ok(Some(DebugOperation::StartApp {
                address: self.image.boot_address,
                boot_type: self.image.boot_type,
            }));
        }
        Ok(None)
    }

    fn complete(&mut self, _completion: DebugCompletion) -> Result<(), FirmwareError> {
        Ok(())
    }
}

pub(super) enum DebugOperation {
    Read { address: u32 },
    Write { address: u32, value: u32 },
    MaskWrite { address: u32, mask: u32, value: u32 },
    BlockWrite { address: u32, bytes: Vec<u8> },
    StartApp { address: u32, boot_type: u32 },
}

impl DebugOperation {
    pub(super) fn block_write(address: u32, bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= UPLOAD_CHUNK_BYTES);
        Self::BlockWrite {
            address,
            bytes: bytes.to_vec(),
        }
    }

    fn into_request(self) -> (LmacRequest, PendingOperation) {
        let (message_id, payload, pending) = match self {
            Self::Read { address } => (
                DBG_MEM_READ_REQ,
                address.to_le_bytes().to_vec(),
                PendingOperation::Read { address },
            ),
            Self::Write { address, value } => {
                let mut payload = Vec::with_capacity(8);
                payload.extend_from_slice(&address.to_le_bytes());
                payload.extend_from_slice(&value.to_le_bytes());
                (
                    DBG_MEM_WRITE_REQ,
                    payload,
                    PendingOperation::Write { address },
                )
            }
            Self::MaskWrite {
                address,
                mask,
                value,
            } => {
                let mut payload = Vec::with_capacity(12);
                payload.extend_from_slice(&address.to_le_bytes());
                payload.extend_from_slice(&mask.to_le_bytes());
                payload.extend_from_slice(&value.to_le_bytes());
                (
                    DBG_MEM_MASK_WRITE_REQ,
                    payload,
                    PendingOperation::MaskWrite { address },
                )
            }
            Self::BlockWrite { address, bytes } => {
                let mut payload = Vec::with_capacity(8 + bytes.len());
                payload.extend_from_slice(&address.to_le_bytes());
                payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                payload.extend_from_slice(&bytes);
                (
                    DBG_MEM_BLOCK_WRITE_REQ,
                    payload,
                    PendingOperation::BlockWrite { address },
                )
            }
            Self::StartApp { address, boot_type } => {
                let mut payload = vec![0; 8];
                payload[..4].copy_from_slice(&address.to_le_bytes());
                payload[4..].copy_from_slice(&boot_type.to_le_bytes());
                (DBG_START_APP_REQ, payload, PendingOperation::StartApp)
            }
        };
        (
            LmacRequest {
                message_id,
                destination: TASK_DBG,
                payload,
            },
            pending,
        )
    }
}

enum PendingOperation {
    Read { address: u32 },
    Write { address: u32 },
    MaskWrite { address: u32 },
    BlockWrite { address: u32 },
    StartApp,
}

pub(super) enum DebugCompletion {
    Read { value: u32 },
    Complete,
}

impl PendingOperation {
    fn parse(self, payload: &[u8]) -> Result<DebugCompletion, FirmwareError> {
        match self {
            Self::Read { address } => {
                let actual = read_u32(payload, 0, "memory read")?;
                verify_address("memory read", address, actual)?;
                Ok(DebugCompletion::Read {
                    value: read_u32(payload, 4, "memory read")?,
                })
            }
            Self::Write { address } => {
                verify_address(
                    "memory write",
                    address,
                    read_u32(payload, 0, "memory write")?,
                )?;
                Ok(DebugCompletion::Complete)
            }
            Self::MaskWrite { address } => {
                verify_address(
                    "masked memory write",
                    address,
                    read_u32(payload, 0, "masked memory write")?,
                )?;
                Ok(DebugCompletion::Complete)
            }
            Self::BlockWrite { address } => {
                verify_address(
                    "block memory write",
                    address,
                    read_u32(payload, 0, "block memory write")?,
                )?;
                Ok(DebugCompletion::Complete)
            }
            Self::StartApp => {
                let status = read_u32(payload, 0, "start application")?;
                if status != 0 {
                    return Err(FirmwareError::StartRejected { status });
                }
                Ok(DebugCompletion::Complete)
            }
        }
    }
}

fn read_u32(bytes: &[u8], offset: usize, operation: &'static str) -> Result<u32, FirmwareError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(FirmwareError::TruncatedConfirmation { operation })?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn verify_address(
    operation: &'static str,
    expected: u32,
    actual: u32,
) -> Result<(), FirmwareError> {
    if actual == expected {
        Ok(())
    } else {
        Err(FirmwareError::ConfirmationAddress {
            operation,
            expected,
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_address(request: &LmacRequest) -> u32 {
        u32::from_le_bytes(request.payload[..4].try_into().unwrap())
    }

    fn h_revision_confirmation(request: &LmacRequest) -> Vec<u8> {
        let address = request_address(request);
        match request.message_id {
            DBG_MEM_READ_REQ => {
                let value: u32 = match address {
                    0x4050_0000 => 0x00c0_0000,
                    0x0000_0020 => 2,
                    0x4050_0148 => 0,
                    0x0001_0164 => 0x0020_0000,
                    0x0001_016c => 0x0021_0000,
                    0x0001_0170 => 0x0022_0000,
                    0x0001_0174 => 0x0023_0000,
                    0x0001_0178 => 0x0024_0000,
                    0x0020_0124 => 0x0300_0000,
                    _ => 0,
                };
                let mut payload = address.to_le_bytes().to_vec();
                payload.extend_from_slice(&value.to_le_bytes());
                payload
            }
            DBG_MEM_WRITE_REQ | DBG_MEM_BLOCK_WRITE_REQ | DBG_MEM_MASK_WRITE_REQ => {
                address.to_le_bytes().to_vec()
            }
            DBG_START_APP_REQ => 0u32.to_le_bytes().to_vec(),
            _ => unreachable!("firmware machine emitted a non-debug request"),
        }
    }

    #[test]
    fn confirmation_address_must_match_the_pending_operation() {
        let mut machine = FirmwareMachine::new(plan(ChipVariant::Aic8801).unwrap());
        assert!(matches!(
            machine.poll(None).unwrap(),
            FirmwarePoll::Request(_)
        ));
        let error = match machine.poll(Some(&0xdead_beefu32.to_le_bytes())) {
            Err(error) => error,
            Ok(_) => panic!("mismatched confirmation address was accepted"),
        };
        assert!(matches!(
            error,
            FirmwareError::ConfirmationAddress {
                expected: RAM_FMAC_FW_ADDR,
                actual: 0xdead_beef,
                ..
            }
        ));
    }

    #[test]
    fn h_revision_calibrates_and_loads_h_patch_before_final_start() {
        let mut machine = FirmwareMachine::new(FirmwarePlan::Dc);
        let mut confirmation = None;
        let mut requests = Vec::new();
        for _ in 0..10_000 {
            match machine.poll(confirmation.as_deref()).unwrap() {
                FirmwarePoll::Request(request) => {
                    let address = request_address(&request);
                    let h_patch = request.message_id == DBG_MEM_BLOCK_WRITE_REQ
                        && address == ROM_FMAC_PATCH_ADDR
                        && request.payload[8..].starts_with(&AIC8800_DC_H[..16]);
                    requests.push((request.message_id, address, h_patch));
                    confirmation = Some(h_revision_confirmation(&request));
                }
                FirmwarePoll::Ready => break,
            }
        }

        let h_patch = requests
            .iter()
            .position(|request| request.2)
            .expect("H revision must select its dedicated patch");
        let calibration_upload = requests
            .iter()
            .position(|request| {
                request.0 == DBG_MEM_BLOCK_WRITE_REQ && request.1 == ROM_FMAC_CALIB_ADDR
            })
            .expect("invalid H misc RAM must upload calibration firmware");
        let calibration_start = requests
            .iter()
            .position(|request| {
                request.0 == DBG_START_APP_REQ && request.1 == ROM_FMAC_CALIB_ADDR + 9
            })
            .expect("H calibration firmware must run through FNCALL");
        let final_start = requests
            .iter()
            .position(|request| request.0 == DBG_START_APP_REQ && request.1 == RAM_FMAC_FW_ADDR)
            .expect("FMAC must start only after H calibration and patching");
        assert!(h_patch < calibration_upload);
        assert!(calibration_upload < calibration_start);
        assert!(calibration_start < final_start);
    }
}
