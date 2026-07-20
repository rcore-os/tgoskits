//! AArch64 compatibility facade and target-specific guest FDT policy.

use alloc::vec::Vec;

use ::core::num::NonZeroU32;

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
        crate::machine::FdtInterruptEncoding::ArmGic,
    )?;
    snapshot.grant_whole_machine_assignment()?;
    grant_preconfigured_provider_resources(&mut snapshot)?;
    Ok(snapshot)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderResourceRequest {
    provider: crate::machine::HostDeviceId,
    kind: crate::machine::HostProviderReferenceKind,
    specifier: Vec<u32>,
}

fn grant_preconfigured_provider_resources(
    snapshot: &mut crate::machine::HostPlatformSnapshot,
) -> crate::machine::MachinePlanResult<()> {
    let requests = provider_resource_requests(snapshot);
    for request in requests {
        let grant = match request.kind {
            crate::machine::HostProviderReferenceKind::Clock
            | crate::machine::HostProviderReferenceKind::ClockConfiguration => {
                capture_fixed_clock(&request)
            }
            crate::machine::HostProviderReferenceKind::Reset => capture_deasserted_reset(&request),
            _ => None,
        };
        if let Some(grant) = grant {
            snapshot.grant_provider_resource(&request.provider, grant)?;
        }
    }
    Ok(())
}

fn provider_resource_requests(
    snapshot: &crate::machine::HostPlatformSnapshot,
) -> Vec<ProviderResourceRequest> {
    let mut requests = Vec::new();
    for dependency in snapshot
        .devices()
        .iter()
        .flat_map(crate::machine::HostDeviceDescriptor::dependencies)
        .filter(|dependency| {
            matches!(
                dependency.reference().kind(),
                crate::machine::HostProviderReferenceKind::Clock
                    | crate::machine::HostProviderReferenceKind::ClockConfiguration
                    | crate::machine::HostProviderReferenceKind::Reset
            )
        })
    {
        let request = ProviderResourceRequest {
            provider: dependency.provider().clone(),
            kind: dependency.reference().kind(),
            specifier: dependency.reference().specifier().to_vec(),
        };
        if !requests.contains(&request) {
            requests.push(request);
        }
    }
    requests
}

fn capture_fixed_clock(
    request: &ProviderResourceRequest,
) -> Option<crate::machine::HostProviderResourceGrant> {
    let selector = single_selector(request)?;
    let device_id = rdrive::fdt_path_to_device_id(request.provider.as_str())?;
    let device = rdrive::get::<rdif_clk::Clk>(device_id).ok()?;
    let clock = device.lock().ok()?;
    if !clock
        .is_enabled(rdif_clk::ClockId::from(selector as usize))
        .ok()?
    {
        return None;
    }
    let rate = clock
        .get_rate(rdif_clk::ClockId::from(selector as usize))
        .ok()
        .and_then(|rate| u32::try_from(rate).ok())
        .and_then(NonZeroU32::new)?;
    Some(crate::machine::HostProviderResourceGrant::fixed_clock(
        request.specifier.clone(),
        rate,
    ))
}

fn capture_deasserted_reset(
    request: &ProviderResourceRequest,
) -> Option<crate::machine::HostProviderResourceGrant> {
    let selector = single_selector(request)?;
    let device_id = rdrive::fdt_path_to_device_id(request.provider.as_str())?;
    let device = rdrive::get::<rdif_reset::Reset>(device_id).ok()?;
    let reset = device.lock().ok()?;
    if reset
        .is_asserted(rdif_reset::ResetId::from(selector))
        .ok()?
    {
        return None;
    }
    Some(crate::machine::HostProviderResourceGrant::deasserted_reset(
        request.specifier.clone(),
    ))
}

fn single_selector(request: &ProviderResourceRequest) -> Option<u32> {
    let [selector] = request.specifier.as_slice() else {
        return None;
    };
    Some(*selector)
}

fn fdt_generation(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

pub(super) fn initrd_start_size_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let ramdisk = ramdisk?;
    Some((ramdisk.load_gpa.as_usize() as u64, ramdisk.size? as u64))
}

pub(super) fn patch_physical_timer_interrupts(fdt_bytes: &[u8]) -> AxVmResult<Vec<u8>> {
    crate::boot::fdt::project_guest_physical_timer_interrupts(fdt_bytes, "arm,armv8-timer")
}

pub fn handle_fdt_operations(
    vm_config: &mut AxVMConfig,
    vm_create_config: &mut axvmconfig::AxVMCrateConfig,
    provider: &dyn BootImageProvider,
) -> AxVmResult<Option<GuestDtbImage>> {
    core::prepare_dtb_guest(vm_config, vm_create_config, provider)
}
