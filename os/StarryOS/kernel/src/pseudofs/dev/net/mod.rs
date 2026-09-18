//! `/dev/net`, holding the `tun` clone device.

mod tun;

use alloc::sync::Arc;

use axfs_ng_vfs::{DeviceId, NodeType};

use self::tun::TunFile;
use crate::pseudofs::{Device, DirMapping, SimpleFs};

/// Linux `TUN_MINOR` on the misc major.
const TUN_DEVICE_ID: DeviceId = DeviceId::new(10, 200);

pub(super) fn net_dir(fs: Arc<SimpleFs>) -> DirMapping {
    let mut dir = DirMapping::new();
    dir.add_dynamic("tun", move || {
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            TUN_DEVICE_ID,
            Arc::new(TunFile::new()),
        )
        .into()
    });
    // `tun_chr_open` gives every open its own file; a cached node would share
    // one attachment between unrelated opens.
    dir.set_cacheable(false);
    dir
}
