//! LoongArch64 implementations of AxVM platform capability hooks.

use std::sync::Arc;

use ax_std::os::arceos::modules::ax_task::IrqNotify;

use super::LoongArch64Arch;
use crate::architecture::{GuestBootPlatform, HostTimePlatform, MachinePlatform};

impl MachinePlatform for LoongArch64Arch {
    const MACHINE_ARCHITECTURE: crate::machine::MachineArchitecture =
        crate::machine::MachineArchitecture::LoongArch64;
}

impl GuestBootPlatform for LoongArch64Arch {
    fn init_guest_boot_resources() {
        super::boot::init();
    }

    fn prepare_guest_boot(
        vm_config: &mut crate::config::AxVMConfig,
        vm_create_config: &mut axvmconfig::GuestConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> crate::AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        if vm_create_config.kernel.effective_boot_protocol() != axvmconfig::VMBootProtocol::Uefi {
            return crate::ax_err!(
                Unsupported,
                "LoongArch AxVisor guests currently require UEFI boot"
            );
        }
        super::boot::prepare_uefi_fdt_config(vm_config, vm_create_config)?;
        Ok(None)
    }
}

impl HostTimePlatform for LoongArch64Arch {
    fn request_timer_deadline(_deadline_ns: u64) {}

    fn register_timer_source(
        _deadline_source: Arc<crate::timer::PublishedTimerDeadline>,
        notify: Arc<IrqNotify>,
    ) {
        ax_std::os::arceos::modules::ax_task::register_timer_callback(move |_| {
            notify.notify_irq();
        });
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}
