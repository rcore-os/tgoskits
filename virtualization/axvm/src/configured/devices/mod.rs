//! AxVM-owned configurable virtual-device models.

mod guest_cpufreq;
mod ivc;
mod virtio_blk;
pub(crate) mod virtio_net;
pub(crate) mod virtio_pci;

/// Placement granularity of one AxVisor-emulated MMIO window.
///
/// The guest layout owns an emulated device by the guest page containing its
/// window (`VmRegionKind::EmulatedDevice`), so two emulated devices must never
/// share a page. A model declares its real register-block `size` but requests
/// this page alignment, so the shared MMIO allocator hands the next device a
/// different page.
pub(super) const EMULATED_MMIO_PLACEMENT_ALIGNMENT: u64 = ax_memory_addr::PAGE_SIZE_4K as u64;

#[cfg(test)]
pub(super) use ivc::IVC_CHANNEL_SHARED_RANGE_SIZE;

pub(super) fn register_devices(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), crate::ConfiguredDeviceError> {
    ivc::register(catalog)?;
    guest_cpufreq::register(catalog)?;
    virtio_blk::register(catalog)?;
    virtio_net::register(catalog)?;
    Ok(())
}

pub use guest_cpufreq::{
    VIRTUAL_CPUFREQ_MAX_PERFORMANCE, VIRTUAL_CPUFREQ_MODEL, VirtualCpuFreqOptions,
};

#[cfg(test)]
mod gated_visibility_tests {
    use axvmconfig::VirtualDeviceRequest;

    use super::*;

    fn probe_model_is_known(model: &str) -> bool {
        let mut catalog = crate::ConfiguredDeviceCatalog::new();
        register_devices(&mut catalog).unwrap();
        let request = VirtualDeviceRequest {
            id: "probe".into(),
            model: model.into(),
            options: Default::default(),
        };
        catalog
            .instantiate_node(&request, &crate::DeviceInstantiationContext::new())
            .is_ok()
    }

    fn unknown_model_error(model: &str) -> crate::ConfiguredDeviceError {
        let mut catalog = crate::ConfiguredDeviceCatalog::new();
        register_devices(&mut catalog).unwrap();
        let request = VirtualDeviceRequest {
            id: "probe".into(),
            model: model.into(),
            options: Default::default(),
        };
        match catalog.instantiate_node(&request, &crate::DeviceInstantiationContext::new()) {
            Ok(_) => panic!("model {model} must stay unknown"),
            Err(error) => error,
        }
    }

    #[test]
    fn virtual_cpufreq_model_registers() {
        assert!(probe_model_is_known("virtual-cpufreq"));
        assert!(matches!(
            unknown_model_error("not-a-model"),
            crate::ConfiguredDeviceError::UnknownVirtualDeviceModel { .. }
        ));
    }
}
