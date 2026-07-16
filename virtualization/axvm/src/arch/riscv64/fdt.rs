//! RISC-V compatibility facade and target-specific guest FDT policy.

use crate::{
    AxVmResult,
    boot::{BootImageProvider, fdt::GuestDtbImage},
    config::AxVMConfig,
};

#[path = "../../boot/fdt/core/mod.rs"]
pub(crate) mod core;

pub use core::{require_host_fdt, try_get_host_fdt, update_fdt};

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: super::capabilities::patch_runtime_fdt,
    }
}

pub(crate) fn host_fdt_bytes() -> Option<&'static [u8]> {
    super::capabilities::host_fdt_bytes()
}

pub fn current_host_platform_snapshot()
-> crate::machine::MachinePlanResult<crate::machine::HostPlatformSnapshot> {
    let bytes = require_host_fdt()?;
    let mut snapshot = crate::machine::HostPlatformSnapshot::from_fdt(
        fdt_generation(bytes),
        bytes,
        crate::machine::FdtInterruptEncoding::FirstCell,
    )?;
    if let Some(console) = snapshot.console_device().cloned() {
        snapshot.grant_console_transfer(console, crate::machine::HostConsoleEvidence::Firmware)?;
    }
    Ok(snapshot)
}

fn fdt_generation(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<GuestDtbImage>> {
    core::prepare_dtb_guest(vm_config, vm_create_config, provider)
}
