//! `/dev/net`, holding the `tun` clone device.

mod tun;

use alloc::sync::Arc;

use axfs_ng_vfs::{DeviceId, NodeType, VfsResult};

use self::tun::TunFile;
use crate::pseudofs::{CachePolicy, Device, NodeRegistry, SimpleFs};

/// Linux `TUN_MINOR` on the misc major.
const TUN_DEVICE_ID: DeviceId = DeviceId::new(10, 200);

pub(super) fn register_nodes(registry: &mut NodeRegistry, fs: Arc<SimpleFs>) -> VfsResult<()> {
    registry.dynamic_node("net/tun", CachePolicy::PerLookup, move || {
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            TUN_DEVICE_ID,
            Arc::new(TunFile::new()),
        )
        .into()
    })
}
