use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

#[cfg(any(feature = "jpeg", feature = "rockchip-sdhci"))]
use rdrive::probe::fdt::FdtInfo;
#[cfg(any(
    feature = "rk3588-pcie",
    feature = "rockchip-dwc-xhci",
    feature = "rockchip-ehci"
))]
use rdrive::probe::fdt::{NodeType, reset_refs};
use rdrive::{
    Device,
    probe::{OnProbeError, fdt::ResetRef},
};

#[derive(Clone)]
pub(crate) struct RockchipResetOps {
    node_name: String,
    name: Option<String>,
    device: Device<rdif_reset::Reset>,
    id: rdif_reset::ResetId,
}

impl RockchipResetOps {
    #[cfg(any(feature = "jpeg", feature = "rockchip-sdhci"))]
    pub(crate) fn from_info(info: &FdtInfo<'_>) -> Result<Vec<Self>, OnProbeError> {
        let refs = info.resets()?;
        Self::from_refs(info.node.name(), refs)
    }

    #[cfg(any(
        feature = "rk3588-pcie",
        feature = "rockchip-dwc-xhci",
        feature = "rockchip-ehci"
    ))]
    pub(crate) fn from_node(node: NodeType<'_>) -> Result<Vec<Self>, OnProbeError> {
        let refs = reset_refs(node)?;
        Self::from_refs(node.name(), refs)
    }

    fn from_refs(node_name: &str, refs: Vec<ResetRef>) -> Result<Vec<Self>, OnProbeError> {
        refs.into_iter()
            .map(|reset| Self::from_ref(node_name, &reset))
            .collect()
    }

    fn from_ref(node_name: &str, reset: &ResetRef) -> Result<Self, OnProbeError> {
        if reset.cells != 1 {
            return Err(OnProbeError::other(format!(
                "[{node_name}] reset {} uses {} cells, only one-cell Rockchip reset selectors are \
                 supported",
                reset_label(reset),
                reset.cells
            )));
        }
        let selector = reset.select().ok_or_else(|| {
            OnProbeError::other(format!(
                "[{node_name}] reset {} has no selector",
                reset_label(reset)
            ))
        })?;
        let provider_id = rdrive::fdt_phandle_to_device_id(reset.phandle).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{node_name}] reset provider phandle {:?} is not populated",
                reset.phandle
            ))
        })?;
        let device = rdrive::get::<rdif_reset::Reset>(provider_id).map_err(|err| {
            OnProbeError::other(format!(
                "[{node_name}] reset provider {:?} has no rdif-reset interface: {err}",
                reset.phandle
            ))
        })?;

        Ok(Self {
            node_name: node_name.to_string(),
            name: reset.name.clone(),
            device,
            id: rdif_reset::ResetId::from(selector),
        })
    }

    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[cfg(any(
        feature = "jpeg",
        feature = "rockchip-dwc-xhci",
        feature = "rockchip-ehci"
    ))]
    pub(crate) fn id(&self) -> rdif_reset::ResetId {
        self.id
    }

    #[cfg(any(
        feature = "rk3588-pcie",
        feature = "rockchip-dwc-xhci",
        feature = "rockchip-sdhci"
    ))]
    pub(crate) fn assert(&self) -> Result<(), OnProbeError> {
        self.with_reset("assert", |reset, id| reset.assert(id))
    }

    #[cfg(any(
        feature = "rk3588-pcie",
        feature = "rockchip-dwc-xhci",
        feature = "rockchip-ehci",
        feature = "rockchip-sdhci"
    ))]
    pub(crate) fn deassert(&self) -> Result<(), OnProbeError> {
        self.with_reset("deassert", |reset, id| reset.deassert(id))
    }

    #[cfg(feature = "jpeg")]
    pub(crate) fn pulse(&self) -> Result<(), OnProbeError> {
        self.with_reset("pulse", |reset, id| reset.reset(id))
    }

    fn with_reset(
        &self,
        operation: &'static str,
        f: impl FnOnce(
            &mut rdif_reset::Reset,
            rdif_reset::ResetId,
        ) -> Result<(), rdif_reset::ResetError>,
    ) -> Result<(), OnProbeError> {
        let mut reset = self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock reset {}: {err}",
                self.node_name,
                self.label()
            ))
        })?;
        f(&mut reset, self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to {operation} reset {}: {err}",
                self.node_name,
                self.label()
            ))
        })
    }

    fn label(&self) -> String {
        match self.name() {
            Some(name) => format!("{name}({:#x})", self.id.raw()),
            None => format!("{:#x}", self.id.raw()),
        }
    }
}

fn reset_label(reset: &ResetRef) -> String {
    match reset.name.as_deref() {
        Some(name) => name.to_string(),
        None => format!("phandle {:?}", reset.phandle),
    }
}
