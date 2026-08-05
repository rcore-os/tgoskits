//! Unsealed runtime construction controlled by each architecture.

use axvm_types::EmulatedDeviceConfig;

use crate::{
    DeviceBuildContext, DeviceBundle, DeviceFactoryRegistry, DeviceManagerResult, DeviceRuntime,
    RuntimeAccessPorts, VmResourcePlan,
};

/// Builds one `DeviceRuntime` without prescribing architecture device order.
pub struct DeviceRuntimeBuilder {
    runtime: DeviceRuntime,
}

impl DeviceRuntimeBuilder {
    /// Creates an unsealed runtime with VM access ports attached.
    pub fn new(access_ports: RuntimeAccessPorts) -> Self {
        let mut runtime = DeviceRuntime::empty();
        runtime.attach_access_ports(access_ports);
        Self { runtime }
    }

    /// Atomically registers an architecture-created bundle.
    pub fn register_bundle(&mut self, bundle: DeviceBundle) -> DeviceManagerResult {
        self.runtime.register_bundle(bundle)
    }

    /// Builds one configured device by consuming its planned claims.
    pub fn build_planned_device(
        &mut self,
        device_id: &str,
        config: &EmulatedDeviceConfig,
        factories: &DeviceFactoryRegistry,
        plan: &VmResourcePlan,
    ) -> DeviceManagerResult {
        let bundle = {
            let claims = plan.claim_device(device_id)?;
            let mut context =
                DeviceBuildContext::planned(self.runtime.interrupt_registry(), claims);
            let bundle = factories.build(config, &mut context)?;
            context.finish(bundle)?
        };
        self.runtime.register_bundle(bundle)
    }

    /// Verifies all claims, seals the topology, and returns the runtime.
    pub fn finish(mut self, plan: &VmResourcePlan) -> DeviceManagerResult<DeviceRuntime> {
        plan.verify_consumed()?;
        self.runtime.seal();
        Ok(self.runtime)
    }
}
