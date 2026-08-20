//! Small capability boundaries implemented by the selected guest architecture.

use std::vec::Vec;

use crate::AxVmResult;

/// Failure to collect one capability from every physical CPU targeted by a VM.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TargetCpuCapabilityError {
    /// The VM has no physical CPU target.
    #[error("no target physical CPU is configured for {capability}")]
    NoTargets { capability: &'static str },
    /// One target CPU did not publish the requested capability.
    #[error("target CPU {cpu_id} has no recorded {capability}")]
    Missing {
        capability: &'static str,
        cpu_id: usize,
    },
}

/// Selects the smallest recorded capability across every target physical CPU.
pub(crate) fn minimum_recorded_target_cpu_capability(
    capability: &'static str,
    vcpu_mappings: &[(usize, Option<usize>, usize)],
    capability_for_cpu: impl FnMut(usize) -> Option<u64>,
) -> Result<u64, TargetCpuCapabilityError> {
    let capabilities =
        recorded_target_cpu_capabilities(capability, vcpu_mappings, capability_for_cpu)?;
    let mut capabilities = capabilities.into_iter();
    let (_, first) = capabilities
        .next()
        .ok_or(TargetCpuCapabilityError::NoTargets { capability })?;
    Ok(capabilities.fold(first, |minimum, (_, value)| minimum.min(value)))
}

pub(crate) fn recorded_target_cpu_capabilities(
    capability: &'static str,
    vcpu_mappings: &[(usize, Option<usize>, usize)],
    mut capability_for_cpu: impl FnMut(usize) -> Option<u64>,
) -> Result<Vec<(usize, u64)>, TargetCpuCapabilityError> {
    let cpu_ids = crate::architecture::ops::target_phys_cpu_ids(vcpu_mappings);
    if cpu_ids.is_empty() {
        return Err(TargetCpuCapabilityError::NoTargets { capability });
    }
    cpu_ids
        .into_iter()
        .map(|cpu_id| {
            capability_for_cpu(cpu_id)
                .map(|value| (cpu_id, value))
                .ok_or(TargetCpuCapabilityError::Missing { capability, cpu_id })
        })
        .collect()
}

/// Maps a missing target-CPU capability into the AxVM host-capability domain.
pub(crate) fn unsupported_target_cpu_capability(
    operation: &'static str,
    error: TargetCpuCapabilityError,
) -> crate::AxVmError {
    crate::AxVmError::unsupported(operation, error)
}

/// Architecture selection for fixed guest machine resources.
pub(crate) trait MachinePlatform {
    const MACHINE_ARCHITECTURE: crate::machine::MachineArchitecture;
}

/// Guest firmware preparation performed before common VM memory loading.
pub(crate) trait GuestBootPlatform {
    fn init_guest_boot_resources() {}

    fn prepare_guest_boot(
        _vm_config: &mut crate::config::AxVMConfig,
        _vm_create_config: &mut axvmconfig::GuestConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> AxVmResult<Option<crate::boot::fdt::GuestDtbImage>> {
        Ok(None)
    }
}

/// Architecture-specific guest image planning layered over common byte loading.
pub(crate) trait BootImagePlatform {
    fn default_boot_firmware_load_gpa(
        _config: &axvmconfig::GuestConfig,
    ) -> Option<axvm_types::GuestPhysAddr> {
        None
    }

    fn load_images_from_memory(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
        images: crate::boot::StaticVmImage,
    ) -> AxVmResult {
        loader.load_standard_images_from_memory(images, Self::load_guest_dtb)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn load_images_from_filesystem(
        loader: &mut crate::boot::images::ImageLoaderCore<'_>,
    ) -> AxVmResult {
        loader.load_standard_images_from_filesystem(Self::load_guest_dtb)
    }

    fn load_guest_dtb(
        _loader: &crate::boot::images::ImageLoaderCore<'_>,
        _dtb: &crate::boot::fdt::GuestDtbImage,
    ) -> AxVmResult {
        Ok(())
    }

    fn is_x86_linux_image_config(
        _config: &axvmconfig::GuestConfig,
        _provider: &dyn crate::boot::BootImageProvider,
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_minimum_includes_every_cpu_in_vcpu_affinity_masks() {
        let mappings = [
            (0, Some((1 << 0) | (1 << 2)), 0),
            (1, None, 1),
            (2, Some((1 << 2) | (1 << 3)), 0),
        ];
        let capabilities = [48, 44, 42, 39];

        assert_eq!(
            minimum_recorded_target_cpu_capability("IPA bits", &mappings, |cpu_id| {
                Some(capabilities[cpu_id])
            }),
            Ok(39)
        );
    }

    #[test]
    fn recorded_minimum_rejects_an_empty_target_set() {
        assert_eq!(
            minimum_recorded_target_cpu_capability("IPA bits", &[], |_| Some(48)),
            Err(TargetCpuCapabilityError::NoTargets {
                capability: "IPA bits",
            })
        );
    }

    #[test]
    fn recorded_minimum_rejects_an_uninitialized_target_cpu() {
        let mappings = [(0, Some((1 << 0) | (1 << 2)), 0)];

        assert_eq!(
            minimum_recorded_target_cpu_capability("IPA bits", &mappings, |cpu_id| {
                [Some(48), None, None][cpu_id]
            }),
            Err(TargetCpuCapabilityError::Missing {
                capability: "IPA bits",
                cpu_id: 2,
            })
        );
    }
}
