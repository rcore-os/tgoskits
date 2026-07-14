//! LoongArch compatibility facade for UEFI guest FDT preparation.

use axvmconfig::{AxVMCrateConfig, VMBootProtocol};

use crate::{AxVmResult, ax_err, config::AxVMConfig};

pub fn init_guest_boot_resources() {
    super::boot::init();
}

pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut AxVMCrateConfig,
) -> AxVmResult {
    if vm_create_config.kernel.effective_boot_protocol() == VMBootProtocol::Uefi {
        return super::boot::prepare_uefi_fdt_config(vm_config, vm_create_config);
    }

    ax_err!(
        Unsupported,
        "LoongArch AxVisor guests currently require UEFI boot"
    )
}
