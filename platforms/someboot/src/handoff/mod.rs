//! Validated boot descriptions, application metadata, and EFI transactions.
//!
//! Consumers are part of ordinary EFI boot. Only transaction production needs
//! `efi-handoff`; no loader or OTA policy is encoded in this module.
use crate::boot_resources::{PhysicalRange, ResourceError};

pub mod protocol;
#[cfg(all(efi, not(target_os = "uefi")))]
pub(crate) mod received;

pub const MAX_CMDLINE: usize = 4095;
pub const MAX_METADATA: usize = 512;

#[derive(Clone, Copy, Debug)]
pub struct ApplicationMetadata<'a> {
    pub id: [u8; 16],
    pub version: u32,
    pub bytes: &'a [u8],
}
impl ApplicationMetadata<'_> {
    pub fn validate(self) -> Result<(), HandoffError> {
        if self.id == [0; 16] || self.version == 0 || self.bytes.len() > MAX_METADATA {
            return Err(HandoffError::InvalidMetadata);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HandoffSpec<'a> {
    pub image: PhysicalRange,
    pub cmdline: Option<&'a str>,
    pub initramfs: Option<&'a [u8]>,
    pub metadata: Option<ApplicationMetadata<'a>>,
}
impl HandoffSpec<'_> {
    pub fn validate(&self) -> Result<(), HandoffError> {
        if !self.image.valid()
            || !self.image.start.is_multiple_of(4096)
            || !self.image.len.is_multiple_of(4096)
        {
            return Err(HandoffError::InvalidTarget);
        }
        if let Some(cmdline) = self.cmdline {
            validate_cmdline(cmdline)?;
        }
        if let Some(metadata) = self.metadata {
            metadata.validate()?;
        }
        Ok(())
    }
}
pub(crate) fn validate_cmdline(cmdline: &str) -> Result<(), HandoffError> {
    if cmdline.len() > MAX_CMDLINE || !cmdline.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
        return Err(HandoffError::InvalidCommandLine);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandoffError {
    #[error("invalid target image range")]
    InvalidTarget,
    #[error("invalid command line")]
    InvalidCommandLine,
    #[error("invalid application metadata")]
    InvalidMetadata,
    #[error("invalid EFI handoff record")]
    InvalidProtocol,
    #[error("unsupported EFI handoff version")]
    UnsupportedVersion,
    #[error("EFI handoff capability is unavailable")]
    Unavailable,
    #[error("another EFI handoff transaction is active")]
    Busy,
    #[error("boot resource error: {0}")]
    Resource(#[from] ResourceError),
    #[error("EFI handoff failed (EFI status {0:#x})")]
    Firmware(usize),
}

cfg_select! {
    all(feature = "efi-handoff", target_os = "uefi") => {
        mod uefi_protocol;
        mod firmware;
        pub(crate) mod transaction;
        pub use firmware::PreparedHandoff;
    }
    all(feature = "efi-handoff", efi, target_arch = "loongarch64") => {
        pub(crate) mod transaction;
        pub use crate::efi_stub::loongarch64::handoff::PreparedHandoff;
    }
    all(
        test,
        target_arch = "x86_64",
        not(target_os = "uefi"),
        feature = "boot-resources"
    ) => {
        mod uefi_protocol;
    }
    _ => {}
}

/// Application metadata from the validated inherited EFI transaction.
/// The data is copied into this image before the shared firmware record is reused.
#[cfg(all(efi, not(target_os = "uefi")))]
pub fn inherited_metadata() -> Option<ApplicationMetadata<'static>> {
    received::metadata()
}

#[cfg(all(
    feature = "efi-handoff",
    efi,
    target_arch = "loongarch64",
    not(target_os = "uefi")
))]
pub use crate::efi_stub::loongarch64::handoff::{current_image, flush_instruction_cache};
