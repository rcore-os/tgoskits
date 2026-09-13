//! Structured device-graph construction failures.

use alloc::string::String;

/// Failure while declaring or sealing a VM device graph.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DeviceGraphError {
    /// A stable node identifier was registered twice.
    #[error("device graph node {node} is registered more than once")]
    DuplicateNode {
        /// Duplicate stable node identifier.
        node: String,
    },
    /// The same typed PCI host key was registered twice.
    #[error("PCI host provider {host} is registered more than once")]
    DuplicatePciHost {
        /// Duplicate provider key.
        host: String,
    },
    /// A PCI endpoint selected a host provider that is not registered.
    #[error("PCI endpoint {endpoint} requires unavailable host {host}")]
    PciHostUnavailable {
        /// Endpoint graph identity.
        endpoint: String,
        /// Missing typed provider key.
        host: String,
    },
    /// A PCI endpoint declaration has no runtime device model.
    #[error("PCI endpoint node {node} requires a runtime device model")]
    PciEndpointRequiresRuntimeModel {
        /// Stable endpoint node identifier.
        node: String,
    },
    /// A PCI host provider has no runtime device model.
    #[error("PCI host provider node {node} requires a runtime device model")]
    PciHostRequiresRuntimeModel {
        /// Stable host node identifier.
        node: String,
    },
    /// A node references a parent or dependency that is absent.
    #[error("device graph node {node} references missing node {dependency}")]
    MissingDependency {
        /// Node containing the invalid edge.
        node: String,
        /// Missing parent or dependency.
        dependency: String,
    },
    /// A node contains the same dependency edge more than once.
    #[error("device graph node {node} declares duplicate dependency {dependency}")]
    DuplicateDependency {
        /// Node containing the duplicate edge.
        node: String,
        /// Duplicate dependency.
        dependency: String,
    },
    /// The graph contains a dependency cycle.
    #[error("device graph contains a dependency cycle involving {node}")]
    DependencyCycle {
        /// Stable identifier of one node left in the cycle.
        node: String,
    },
    /// A runtime-backed node has no factory.
    #[error("runtime-backed device graph node {node} has no factory")]
    MissingFactory {
        /// Stable node identifier.
        node: String,
    },
    /// A firmware-only node unexpectedly has a runtime factory.
    #[error("firmware-only device graph node {node} cannot have a runtime factory")]
    FirmwareFactory {
        /// Stable node identifier.
        node: String,
    },
    /// Factory declaration failed for one node.
    #[error("device graph node {node} declaration failed: {detail}")]
    Declaration {
        /// Stable node identifier.
        node: String,
        /// Underlying typed-device error rendered at the graph boundary.
        detail: String,
    },
}
