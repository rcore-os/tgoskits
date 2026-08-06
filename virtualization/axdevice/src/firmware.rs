//! Device-owned guest firmware descriptions.

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use crate::{DeviceNodeId, ResolvedDeviceGraph, ResolvedDeviceResources};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FdtNodeSpec {
    pub path: String,
    pub properties: Vec<FirmwareProperty>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpiDeviceSpec {
    pub path: String,
    pub aml: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareProperty {
    pub name: String,
    pub value: Vec<u8>,
}

pub trait FdtNodeModel: Send + Sync {
    fn render(
        &self,
        resources: &ResolvedDeviceResources,
    ) -> Result<FdtNodeSpec, FirmwareBuildError>;
}

pub trait AcpiNodeModel: Send + Sync {
    fn render(
        &self,
        resources: &ResolvedDeviceResources,
    ) -> Result<AcpiDeviceSpec, FirmwareBuildError>;
}

#[derive(Clone, Default)]
pub struct FirmwareModels {
    pub fdt: Option<Arc<dyn FdtNodeModel>>,
    pub acpi: Option<Arc<dyn AcpiNodeModel>>,
}

/// Device-owned firmware fragments rendered from one resolved graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedFirmwareModels {
    fdt: Vec<(DeviceNodeId, FdtNodeSpec)>,
    acpi: Vec<(DeviceNodeId, AcpiDeviceSpec)>,
}

impl RenderedFirmwareModels {
    /// Returns FDT fragments in graph dependency order.
    pub fn fdt(&self) -> &[(DeviceNodeId, FdtNodeSpec)] {
        &self.fdt
    }

    /// Returns ACPI fragments in graph dependency order.
    pub fn acpi(&self) -> &[(DeviceNodeId, AcpiDeviceSpec)] {
        &self.acpi
    }
}

/// Renders every node capability against the graph's canonical resources.
pub fn render_device_firmware(
    graph: &ResolvedDeviceGraph,
) -> Result<RenderedFirmwareModels, FirmwareBuildError> {
    let mut rendered = RenderedFirmwareModels::default();
    for node in graph.nodes() {
        let resources =
            graph
                .resources_for(node.id())
                .map_err(|error| FirmwareBuildError::InvalidModel {
                    node: node.id().as_str().into(),
                    detail: error.to_string(),
                })?;
        if let Some(model) = &node.firmware_models().fdt {
            rendered
                .fdt
                .push((node.id().clone(), model.render(resources)?));
        }
        if let Some(model) = &node.firmware_models().acpi {
            rendered
                .acpi
                .push((node.id().clone(), model.render(resources)?));
        }
    }
    Ok(rendered)
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FirmwareBuildError {
    #[error("invalid firmware model for {node}: {detail}")]
    InvalidModel { node: String, detail: String },
    #[error("firmware target is unsupported for {node}: {target}")]
    UnsupportedTarget { node: String, target: &'static str },
}
