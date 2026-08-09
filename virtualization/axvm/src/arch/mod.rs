//! Target architecture selection and stable internal dispatch.

pub(crate) use crate::architecture::*;
use crate::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::Aarch64Arch as CurrentArch;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::Aarch64VmPlan as ArchVmPlan;
#[cfg(target_arch = "aarch64")]
pub use aarch64::ImageLoader;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::fdt;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::LoongArch64Arch as CurrentArch;
#[cfg(target_arch = "loongarch64")]
pub(crate) use loongarch64::LoongArchVmPlan as ArchVmPlan;
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
pub(crate) use riscv64::RiscvVmPlan as ArchVmPlan;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::fdt;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::X86_64Arch as CurrentArch;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::X86VmPlan as ArchVmPlan;
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
    #[cfg(all(
        any(
            target_arch = "aarch64",
            target_arch = "x86_64",
            target_arch = "loongarch64",
            target_arch = "riscv64"
        ),
        any(feature = "fs", feature = "host-fs")
    ))]
    pub use crate::host::arceos::shutdown_host_filesystems;
}

pub(crate) type ArchVCpu = <CurrentArch as ArchOps>::VCpu;
pub(crate) type ArchPerCpu = <CurrentArch as ArchOps>::PerCpu;
pub(crate) type ArchNestedPageTable = <CurrentArch as ArchOps>::NestedPageTable;

pub(crate) fn register_timer_source(
    deadline_source: std::sync::Arc<crate::timer::PublishedTimerDeadline>,
    notify: std::sync::Arc<ax_std::os::arceos::modules::ax_task::IrqNotify>,
) {
    CurrentArch::register_timer_source(deadline_source, notify);
}

pub(crate) fn request_timer_deadline(deadline_ns: u64) {
    CurrentArch::request_timer_deadline(deadline_ns);
}

pub(crate) fn init_guest_boot_resources() {
    CurrentArch::init_guest_boot_resources();
}

pub(crate) fn prepare_guest_boot(
    vm_config: &mut crate::config::AxVMConfig,
    vm_create_config: &mut axvmconfig::GuestConfig,
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
    config: &axvmconfig::GuestConfig,
    provider: &dyn crate::boot::BootImageProvider,
) -> bool {
    CurrentArch::is_x86_linux_image_config(config, provider)
}

pub(crate) fn default_boot_firmware_load_gpa(
    config: &axvmconfig::GuestConfig,
) -> Option<axvm_types::GuestPhysAddr> {
    CurrentArch::default_boot_firmware_load_gpa(config)
}

#[cfg(any(target_arch = "riscv64", test))]
pub(crate) fn riscv_hart_mask_targets(
    hart_mask: usize,
    hart_mask_base: usize,
    vcpu_mappings: impl IntoIterator<Item = (usize, Option<usize>, usize)>,
) -> crate::CpuMask<64> {
    let mut targets = crate::CpuMask::new();

    for (vcpu_id, _, phys_id) in vcpu_mappings {
        // CpuMask<64> cannot represent a local vCPU ID >= 64.
        if vcpu_id >= 64 {
            continue;
        }

        // SBI uses ULONG_MAX as the all-harts selector.
        if hart_mask_base == usize::MAX {
            targets.set(vcpu_id, true);
            continue;
        }

        // A hart below the requested base is not selected.
        let Some(bit) = phys_id.checked_sub(hart_mask_base) else {
            continue;
        };

        // Ignore mask bits that cannot exist on this host.
        if bit >= usize::BITS as usize {
            continue;
        }

        if ((hart_mask >> bit) & 1) != 0 {
            targets.set(vcpu_id, true);
        }
    }

    targets
}

/// Delivers a computed IPI target mask to the current and remote vCPUs.
///
/// This helper is shared by the production RISC-V SEND_IPI path and tests,
/// so tests cover the same split between local HVIP injection and remote queueing.
#[cfg(any(target_arch = "riscv64", test))]
pub(crate) fn deliver_riscv_ipi_targets<E>(
    targets: crate::CpuMask<64>,
    current_vcpu_id: usize,
    vector: usize,
    mut inject_current: impl FnMut(usize) -> Result<(), E>,
    mut inject_remote: impl FnMut(crate::CpuMask<64>, usize) -> Result<(), E>,
) -> Result<(), E> {
    if current_vcpu_id < 64 && targets.get(current_vcpu_id) {
        inject_current(vector)?;
    }

    let mut remote_targets = targets;
    if current_vcpu_id < 64 {
        remote_targets.set(current_vcpu_id, false);
    }

    if !remote_targets.is_empty() {
        inject_remote(remote_targets, vector)?;
    }

    Ok(())
}

#[cfg(test)]
mod riscv_hart_mask_tests {
    use super::*;

    #[test]
    fn legacy_hart_mask_routes_sparse_guest_hart_to_local_vcpu() {
        let mappings = [
            (0usize, None, 4usize),
            (1usize, None, 9usize),
            (2usize, None, 5usize),
        ];

        let targets = riscv_hart_mask_targets(1usize << 5, 0, mappings);

        assert!(targets.get(2));
        assert!(!targets.get(0));
        assert!(!targets.get(1));
    }

    #[test]
    fn standard_hart_mask_uses_non_zero_base_before_mapping_to_local_vcpu() {
        let mappings = [
            (0usize, None, 4usize),
            (1usize, None, 9usize),
            (2usize, None, 5usize),
        ];

        let targets = riscv_hart_mask_targets(1usize << 1, 4, mappings);

        assert!(targets.get(2));
        assert!(!targets.get(0));
        assert!(!targets.get(1));
    }

    #[test]
    fn standard_hart_mask_base_max_targets_all_vcpus() {
        let mappings = [
            (0usize, None, 4usize),
            (1usize, None, 9usize),
            (2usize, None, 5usize),
        ];

        let targets = riscv_hart_mask_targets(0, usize::MAX, mappings);

        assert!(targets.get(0));
        assert!(targets.get(1));
        assert!(targets.get(2));
    }
}

#[cfg(test)]
mod standard_hart_mask_mapping_tests {
    use super::*;

    #[test]
    fn standard_hart_mask_base_maps_guest_hart_to_local_vcpu() {
        // local vCPU 0/1/2 correspond to guest hart IDs 4/5/9.
        let mappings = std::vec![
            (0usize, None, 4usize),
            (1usize, None, 5usize),
            (2usize, None, 9usize),
        ];

        // base=4, bit 1 selects guest hart 5 only.
        let targets = riscv_hart_mask_targets(1usize << 1, 4, mappings);

        assert!(!targets.get(0));
        assert!(targets.get(1));
        assert!(!targets.get(2));
    }
}

#[cfg(test)]
mod riscv_ipi_delivery_boundary_tests {
    use super::*;

    #[test]
    fn out_of_range_vcpu_id_is_ignored() {
        let mappings = [(0usize, None, 5usize), (64usize, None, 5usize)];

        let targets = riscv_hart_mask_targets(1usize << 5, 0, mappings);

        assert!(targets.get(0));
        assert!(!targets.get(1));
    }

    #[test]
    fn hart_below_base_is_ignored() {
        let mappings = [(0usize, None, 3usize), (1usize, None, 5usize)];

        let targets = riscv_hart_mask_targets(1usize << 1, 4, mappings);

        assert!(!targets.get(0));
        assert!(targets.get(1));
    }

    #[test]
    fn send_ipi_routes_only_selected_remote_vcpu() {
        let mut targets = crate::CpuMask::<64>::new();
        targets.set(2, true);

        let mut current_count = 0usize;
        let mut remote_mask = crate::CpuMask::<64>::new();

        deliver_riscv_ipi_targets(
            targets,
            0,
            1,
            |_| -> Result<(), ()> {
                current_count += 1;
                Ok(())
            },
            |mask, vector| -> Result<(), ()> {
                assert_eq!(vector, 1);
                remote_mask = mask;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(current_count, 0);
        assert!(remote_mask.get(2));
        assert!(!remote_mask.get(0));
        assert!(!remote_mask.get(1));
    }

    #[test]
    fn send_ipi_injects_current_vcpu_only_when_selected() {
        let mut targets = crate::CpuMask::<64>::new();
        targets.set(0, true);
        targets.set(2, true);

        let mut current_count = 0usize;
        let mut remote_mask = crate::CpuMask::<64>::new();

        deliver_riscv_ipi_targets(
            targets,
            0,
            1,
            |_| -> Result<(), ()> {
                current_count += 1;
                Ok(())
            },
            |mask, _| -> Result<(), ()> {
                remote_mask = mask;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(current_count, 1);
        assert!(remote_mask.get(2));
        assert!(!remote_mask.get(0));
    }
}
