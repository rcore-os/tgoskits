//! Declarative VM device graph and its resolved resource view.

mod builder;
mod error;
mod node;
mod resolved;

pub use builder::{DeclaredDeviceGraph, DeviceGraphBuilder};
pub use error::DeviceGraphError;
pub use node::{
    DeviceFirmwareBinding, DeviceNodeId, DeviceNodeKind, DeviceNodeSpec, HostPassthroughMapping,
};
pub use resolved::{ResolvedDeviceGraph, ResolvedDeviceNode};
