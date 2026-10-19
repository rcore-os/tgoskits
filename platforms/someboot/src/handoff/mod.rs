//! Validated boot descriptions, application metadata, and EFI transactions.
//!
//! Consumers are part of ordinary EFI boot. Only transaction production needs
//! `efi-handoff`; no loader or OTA policy is encoded in this module.
use crate::boot_resources::{PhysicalRange, ResourceError};

pub(crate) mod cmdline;
pub(crate) mod memory;
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
    /// Additional owned allocations which must survive beyond target takeover.
    pub preserve: &'a [PhysicalRange],
}
impl HandoffSpec<'_> {
    pub fn validate(&self) -> Result<(), HandoffError> {
        if !self.image.valid()
            || !self.image.start.is_multiple_of(4096)
            || !self.image.len.is_multiple_of(4096)
        {
            return Err(HandoffError::InvalidTarget);
        }
        if self.preserve.len() > crate::boot_resources::MAX_ALLOCATIONS
            || self.preserve.iter().any(|r| !r.valid())
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
pub(crate) use cmdline::validate as validate_cmdline;

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
        pub use crate::efi_stub::loongarch64::handoff::{PreparedHandoff, SealedHandoff};
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

/// Physical backing still reachable by the stopped caller's execution context.
/// Construction and insertion do not allocate. Adjacent ranges are coalesced.
pub struct ExecutionSnapshot {
    ranges: [PhysicalRange; protocol::MAX_REGIONS],
    len: usize,
}
impl Default for ExecutionSnapshot {
    fn default() -> Self {
        Self::new()
    }
}
impl ExecutionSnapshot {
    pub const fn new() -> Self {
        Self {
            ranges: [PhysicalRange::new(0, 0); protocol::MAX_REGIONS],
            len: 0,
        }
    }
    pub fn protect(&mut self, range: PhysicalRange) -> Result<(), HandoffError> {
        let start = range.start & !4095;
        let end = range
            .end()
            .and_then(|v| v.checked_add(4095))
            .ok_or(HandoffError::InvalidProtocol)?
            & !4095;
        if start == 0 || end <= start {
            return Err(HandoffError::InvalidProtocol);
        }
        let mut merged = PhysicalRange::new(start, end - start);
        let mut i = 0;
        while i < self.len {
            let r = self.ranges[i];
            if r.start <= merged.end().unwrap() && merged.start <= r.end().unwrap() {
                let a = r.start.min(merged.start);
                let b = r.end().unwrap().max(merged.end().unwrap());
                merged = PhysicalRange::new(a, b - a);
                self.len -= 1;
                self.ranges[i] = self.ranges[self.len];
            } else {
                i += 1;
            }
        }
        if self.len == self.ranges.len() {
            return Err(HandoffError::InvalidProtocol);
        }
        self.ranges[self.len] = merged;
        self.len += 1;
        Ok(())
    }
    pub fn ranges(&self) -> &[PhysicalRange] {
        &self.ranges[..self.len]
    }
}

/// Accounting only: these pages have already been transferred to normal RAM.
/// The ranges do not grant ownership or permission to access their contents.
#[derive(Clone, Copy, Debug)]
pub struct ReclaimedMemory {
    pub range: PhysicalRange,
    pub resource_bytes: u64,
}
#[cfg(all(efi, target_arch = "loongarch64", not(target_os = "uefi")))]
impl ReclaimedMemory {
    pub(crate) const EMPTY: Self = Self {
        range: PhysicalRange::new(0, 0),
        resource_bytes: 0,
    };
}
/// Previous-stage ranges released when this image took over its CPU context.
#[cfg(all(efi, target_arch = "loongarch64", not(target_os = "uefi")))]
pub fn reclaimed_memory() -> &'static [ReclaimedMemory] {
    received::loongarch64::reclaimed_memory()
}
/// Whether the receiver has irreversibly replaced the previous execution context.
pub fn takeover_completed() -> bool {
    cfg_select! {
        all(efi, target_arch = "loongarch64", not(target_os = "uefi")) => {
            received::loongarch64::takeover_completed()
        }
        _ => false,
    }
}
/// A consumed archive's completed transfer into the general allocator, if any.
#[cfg(all(efi, target_arch = "loongarch64", not(target_os = "uefi")))]
pub fn reclaimed_initramfs() -> Option<PhysicalRange> {
    crate::boot_payload::returned_pages()
}

/// Physical image of the immediately preceding stage; accounting only.
#[cfg(all(efi, target_arch = "loongarch64", not(target_os = "uefi")))]
pub fn previous_image() -> Option<PhysicalRange> {
    Some(received::loongarch64::inherited_payload()?.caller_image)
}
