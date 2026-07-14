//! Target architecture selection and stable internal dispatch.

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use axvm_types::{VCpuId, VMId};

pub(crate) use crate::architecture::*;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use crate::task::AsVCpuTask;
use crate::{
    AxVmResult,
    architecture::{BootImagePlatform, GuestBootPlatform},
};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(test)]
mod riscv_forwarding_contract_tests {
    mod completion_restore {
        include!("riscv64/completion_restore.rs");
    }

    mod forwarded_ingress {
        include!("riscv64/forwarded_ingress.rs");
    }

    mod owner_doorbell {
        include!("riscv64/owner_doorbell.rs");
    }

    mod route_transaction {
        include!("riscv64/route_transaction.rs");
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::Aarch64Arch as CurrentArch;
#[cfg(target_arch = "aarch64")]
pub use aarch64::ImageLoader;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::fdt;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::LoongArch64Arch as CurrentArch;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::boot as guest_platform;
#[cfg(target_arch = "loongarch64")]
pub use loongarch64::boot::ImageLoader;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::fdt;
#[cfg(not(target_arch = "loongarch64"))]
pub(crate) mod guest_platform {
    #[doc(hidden)]
    pub const SUPPORTED: bool = false;
}
#[cfg(target_arch = "riscv64")]
pub use riscv64::ImageLoader;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::Riscv64Arch as CurrentArch;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::fdt;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::X86_64Arch as CurrentArch;
#[cfg(target_arch = "x86_64")]
pub use x86_64::boot::ImageLoader;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::fdt;

/// Architecture-specific public compatibility exports.
pub mod platform {
    #[cfg(target_arch = "aarch64")]
    pub use super::aarch64::{host_fdt_bootarg, host_phys_to_virt};
    #[cfg(target_arch = "loongarch64")]
    pub use super::loongarch64::irq::{
        register_guest_irq_route as register_loongarch_guest_irq_route,
        unregister_guest_irq_routes as unregister_loongarch_guest_irq_routes,
    };
    #[cfg(target_arch = "loongarch64")]
    pub use super::loongarch64::{host_fdt_bootarg, host_phys_to_virt};
    #[cfg(target_arch = "riscv64")]
    pub use super::riscv64::{host_fdt_bootarg, host_phys_to_virt};
    #[cfg(target_arch = "x86_64")]
    pub use super::x86_64::irq::{
        register_ioapic_irq_forwarding_activator as register_x86_ioapic_irq_forwarding_activator,
        register_ioapic_irq_forwarding_route as register_x86_ioapic_irq_forwarding_route,
        register_ioapic_irq_forwarding_route_with_trigger as register_x86_ioapic_irq_forwarding_route_with_trigger,
    };
    #[cfg(any(feature = "fs", feature = "host-fs"))]
    pub use crate::host::arceos::shutdown_host_filesystems;
}

pub(crate) type ArchVCpu = <CurrentArch as ArchOps>::VCpu;
pub(crate) type ArchPerCpu = <CurrentArch as ArchOps>::PerCpu;
pub(crate) type ArchNestedPageTable = <CurrentArch as ArchOps>::NestedPageTable;

/// Logical vCPU identity available to architecture device callbacks.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VcpuExecutionIdentity {
    vm_id: VMId,
    vcpu_id: VCpuId,
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
impl VcpuExecutionIdentity {
    const fn new(vm_id: VMId, vcpu_id: VCpuId) -> Self {
        Self { vm_id, vcpu_id }
    }

    pub(crate) const fn into_ids(self) -> (VMId, VCpuId) {
        (self.vm_id, self.vcpu_id)
    }
}

/// Resolves the logical vCPU for normal architecture device emulation.
///
/// A live CPU-local publication wins while the backend is bound. After
/// unbind, the current vCPU host thread extension supplies the same logical
/// identity without pinning the task. Hard IRQ code never consults that task
/// extension and receives `None` when no live publication exists.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) fn current_vcpu_identity_for_task() -> Option<VcpuExecutionIdentity> {
    let live_identity = crate::vcpu::current_vcpu_identity()
        .map(|identity| VcpuExecutionIdentity::new(identity.vm_id(), identity.vcpu_id()));
    select_vcpu_execution_identity(live_identity, crate::host::task::in_hard_irq(), || {
        let current = crate::host::task::try_current_task()?;
        let task = current.try_as_vcpu_task()?;
        Some(VcpuExecutionIdentity::new(
            task.vcpu.vm_id(),
            task.vcpu.id(),
        ))
    })
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn select_vcpu_execution_identity(
    live_identity: Option<VcpuExecutionIdentity>,
    in_hard_irq: bool,
    task_identity: impl FnOnce() -> Option<VcpuExecutionIdentity>,
) -> Option<VcpuExecutionIdentity> {
    if live_identity.is_some() {
        return live_identity;
    }
    if in_hard_irq {
        return None;
    }
    task_identity()
}

pub(crate) fn init_guest_boot_resources() {
    CurrentArch::init_guest_boot_resources();
}

pub(crate) fn prepare_guest_boot(
    vm_config: &mut crate::config::AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
    CurrentArch::prepare_guest_boot(vm_config, vm_create_config, provider)
}

pub(crate) fn load_images_from_memory(
    loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    images: crate::boot::StaticVmImage,
) -> AxVmResult {
    CurrentArch::load_images_from_memory(loader, images)
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
pub(crate) fn load_images_from_filesystem(
    loader: &mut crate::boot::images::ImageLoaderCore<'_>,
) -> AxVmResult {
    CurrentArch::load_images_from_filesystem(loader)
}

pub(crate) fn is_x86_linux_image_config(
    config: &axvmconfig::AxVMCrateConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> bool {
    CurrentArch::is_x86_linux_image_config(config, provider)
}

pub(crate) fn default_boot_firmware_load_gpa(
    config: &axvmconfig::AxVMCrateConfig,
) -> Option<axvm_types::GuestPhysAddr> {
    CurrentArch::default_boot_firmware_load_gpa(config)
}

#[cfg(all(test, any(target_arch = "aarch64", target_arch = "x86_64")))]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn task_identity_selection_prefers_live_bound_publication() {
        let fallback_calls = AtomicUsize::new(0);
        let live = VcpuExecutionIdentity::new(3, 1);
        let fallback = VcpuExecutionIdentity::new(3, 2);

        let selected = select_vcpu_execution_identity(Some(live), false, || {
            fallback_calls.fetch_add(1, Ordering::Relaxed);
            Some(fallback)
        });

        assert_eq!(selected, Some(live));
        assert_eq!(fallback_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn task_identity_selection_falls_back_after_backend_unbind() {
        let fallback = VcpuExecutionIdentity::new(3, 2);

        let selected = select_vcpu_execution_identity(None, false, || Some(fallback));

        assert_eq!(selected, Some(fallback));
    }

    #[test]
    fn task_identity_selection_returns_none_for_non_vcpu_thread() {
        let selected = select_vcpu_execution_identity(None, false, || None);

        assert_eq!(selected, None);
    }

    #[test]
    fn task_identity_selection_never_falls_back_in_hard_irq() {
        let fallback_calls = AtomicUsize::new(0);

        let selected = select_vcpu_execution_identity(None, true, || {
            fallback_calls.fetch_add(1, Ordering::Relaxed);
            Some(VcpuExecutionIdentity::new(3, 2))
        });

        assert_eq!(selected, None);
        assert_eq!(fallback_calls.load(Ordering::Relaxed), 0);
    }
}
