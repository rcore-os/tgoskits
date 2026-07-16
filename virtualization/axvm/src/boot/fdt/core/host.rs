//! Access to the immutable host FDT captured at platform boot.

use crate::machine::MachinePlanError;

pub fn try_get_host_fdt() -> Option<&'static [u8]> {
    super::super::host_fdt_bytes().inspect(|bytes| {
        trace!("Host FDT size: 0x{:x}", bytes.len());
    })
}

pub fn require_host_fdt() -> Result<&'static [u8], MachinePlanError> {
    try_get_host_fdt().ok_or_else(|| MachinePlanError::InvalidFirmware {
        detail: "host FDT is unavailable for passthrough machine planning".into(),
    })
}
