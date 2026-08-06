//! Runtime construction model owned by one device-graph node.

use crate::*;

/// Declares and builds one concrete virtual-device instance.
///
/// A model owns its validated, type-specific configuration. The same object is
/// retained from declaration through construction, so the resource plan cannot
/// be paired with a different configuration at build time.
pub trait DeviceModel: Send + Sync {
    /// Declares all named resources required by this instance.
    fn declare(&self) -> DeviceManagerResult<DeviceDeclaration>;

    /// Builds the device while consuming only resources issued by the plan.
    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle>;
}
