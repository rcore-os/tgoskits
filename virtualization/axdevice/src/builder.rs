//! Unsealed runtime construction controlled by each architecture.

use alloc::sync::Arc;

use crate::*;

/// Builds one `DeviceRuntime` without prescribing architecture device order.
pub struct DeviceRuntimeBuilder {
    runtime: DeviceRuntime,
    stage2_remap: Option<Arc<dyn Stage2Remap>>,
}

impl DeviceRuntimeBuilder {
    /// Creates an unsealed runtime with VM access ports attached.
    pub fn new(access_ports: RuntimeAccessPorts) -> Self {
        let mut runtime = DeviceRuntime::empty();
        runtime.attach_access_ports(access_ports);
        Self {
            runtime,
            stage2_remap: None,
        }
    }

    /// Attaches the VM-wide stage-2 update port for direct-mapping devices.
    pub fn with_stage2_remap(mut self, remap: Arc<dyn Stage2Remap>) -> Self {
        self.stage2_remap = Some(remap);
        self
    }

    /// Atomically registers an architecture-created bundle.
    pub fn register_bundle(&mut self, bundle: DeviceBundle) -> DeviceManagerResult {
        self.runtime.register_bundle(bundle)
    }

    /// Builds one resolved graph node with the exact model that declared it.
    pub fn build_graph_node(
        &mut self,
        node: &ResolvedDeviceNode,
        plan: &VmResourcePlan,
    ) -> DeviceManagerResult {
        let Some(model) = node.model() else {
            return Ok(());
        };
        let bundle = {
            let claims = plan.claim_device(node.id().as_str())?;
            let mut context = DeviceBuildContext::planned(
                self.runtime.interrupt_registry(),
                claims,
                node.pci_host_topology(),
                self.stage2_remap.clone(),
                node.id().clone(),
            );
            let bundle = model.build(&mut context)?;
            context.finish(bundle)?
        };
        self.runtime.register_graph_bundle(node, bundle)
    }

    /// Verifies all claims, seals the topology, and returns the runtime.
    pub fn finish(mut self, plan: &VmResourcePlan) -> DeviceManagerResult<DeviceRuntime> {
        plan.verify_consumed()?;
        self.runtime.seal();
        Ok(self.runtime)
    }
}
