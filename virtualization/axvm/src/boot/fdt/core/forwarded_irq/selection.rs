//! Canonical host-FDT passthrough producer selection.

use alloc::{string::String, vec::Vec};

use fdt_edit::Fdt;

use super::super::device;
use crate::{AxVmError, AxVmResult, ForwardedIrqConfigError, config::AxVMConfig};

pub(super) fn selected_producer_paths(
    config: &AxVMConfig,
    host_fdt: &Fdt,
) -> AxVmResult<Vec<String>> {
    let selections = config
        .pass_through_devices()
        .iter()
        .map(|device| device.name.as_str())
        .collect::<Vec<_>>();
    for selection in &selections {
        if !selection.starts_with('/') || host_fdt.get_by_path_id(selection).is_none() {
            return Err(AxVmError::ForwardedIrqConfig {
                source: ForwardedIrqConfigError::InvalidSelection {
                    selection: (*selection).into(),
                },
            });
        }
    }

    Ok(device::find_all_passthrough_devices(config, host_fdt))
}
