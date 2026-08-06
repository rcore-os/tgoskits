use alloc::string::String;
pub use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use fdt_edit::NodeId;
pub use rdif_base::irq::IrqConfig;

use crate::custom_id;

custom_id!(DeviceId, u64);
custom_id!(DriverId, u64);

#[derive(Default, Debug, Clone)]
pub struct Descriptor {
    pub(crate) device_id: DeviceId,
    pub name: &'static str,
    pub irq_parent: Option<DeviceId>,
    pub(crate) fdt_node: Option<FdtNodeIdentity>,
    // pub irqs: Vec<IrqConfig>,
}

/// Stable firmware identity for a device originating from an FDT node.
#[derive(Debug, Clone)]
pub struct FdtNodeIdentity {
    node_id: NodeId,
    path: String,
}

impl FdtNodeIdentity {
    pub(crate) fn new(node_id: NodeId, path: String) -> Self {
        Self { node_id, path }
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Descriptor {
    pub fn new() -> Self {
        Self {
            device_id: DeviceId::new(),
            ..Default::default()
        }
    }
}

impl Descriptor {
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn fdt_node(&self) -> Option<&FdtNodeIdentity> {
        self.fdt_node.as_ref()
    }
}

static ITER: AtomicU64 = AtomicU64::new(0);

impl DeviceId {
    pub fn new() -> Self {
        Self(ITER.fetch_add(1, Ordering::SeqCst))
    }
}

macro_rules! impl_driver_id_for {
    ($t:ty) => {
        impl From<$t> for DriverId {
            fn from(value: $t) -> Self {
                Self(value as _)
            }
        }
    };
}

impl_driver_id_for!(usize);
impl_driver_id_for!(u32);
