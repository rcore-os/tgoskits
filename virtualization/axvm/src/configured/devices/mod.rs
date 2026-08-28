//! AxVM-owned configurable virtual-device models.

mod ivc;
mod virtio_blk;
mod virtio_net;
#[cfg(feature = "vpci-test-device")]
mod vpci_test;

#[cfg(test)]
pub(super) use ivc::IVC_CHANNEL_SHARED_RANGE_SIZE;

pub(super) fn register_devices(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), crate::ConfiguredDeviceError> {
    ivc::register(catalog)?;
    virtio_blk::register(catalog)?;
    virtio_net::register(catalog)?;
    #[cfg(feature = "vpci-test-device")]
    vpci_test::register(catalog)?;
    Ok(())
}

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

    #[cfg(feature = "vpci-test-device")]
    #[test]
    fn vpci_test_model_registers_under_its_feature() {
        assert!(probe_model_is_known("vpci-test"));
        assert!(matches!(
            unknown_model_error("not-a-model"),
            crate::ConfiguredDeviceError::UnknownVirtualDeviceModel { .. }
        ));
    }

    #[cfg(not(feature = "vpci-test-device"))]
    #[test]
    fn vpci_test_model_stays_unknown_without_its_feature() {
        assert!(!probe_model_is_known("vpci-test"));
        assert!(matches!(
            unknown_model_error("vpci-test"),
            crate::ConfiguredDeviceError::UnknownVirtualDeviceModel { .. }
        ));
    }
}
