//! `/dev/net` directory.
//!
//! Holds the single `tun` clone device (`/dev/net/tun`). Each `open` of that
//! node yields a fresh, unattached [`TunFile`]; userspace binds it to an
//! interface with `TUNSETIFF`.

mod tun;

use alloc::sync::Arc;

use axfs_ng_vfs::{DeviceId, NodeType};

pub use self::tun::TunFile;
#[cfg(axtest)]
pub(crate) use self::tun::{
    tun_rollback_destroys_created_device_for_test, tun_rollback_detaches_existing_device_for_test,
    tun_rollback_on_concurrent_close_for_test,
};
use crate::pseudofs::{Device, DirMapping, SimpleFs};

/// TUN driver device id: Linux's misc-device major 10, minor 200
/// (`TUN_MINOR`), exposed at `/dev/net/tun`.
const TUN_DEVICE_ID: DeviceId = DeviceId::new(10, 200);

/// Builds the `/dev/net` directory contents.
pub fn net_dir(fs: Arc<SimpleFs>) -> DirMapping {
    let mut dir = DirMapping::new();
    dir.add_dynamic("tun", {
        let fs = fs.clone();
        move || {
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                TUN_DEVICE_ID,
                Arc::new(TunFile::new()),
            )
            .into()
        }
    });
    // `/dev/net/tun` is a clone device: Linux `tun_chr_open` allocates a fresh
    // `tun_file` per `open(2)`, and every `TunFile` here holds per-open state
    // (attachment, negotiated flags, the closing latch). A cacheable directory
    // would memoize the first lookup and hand every opener the same `TunFile`,
    // so one fd's close would leave its `closing` latch set for the next opener
    // and concurrent openers would share one attachment. Opting out of caching
    // re-runs the maker per lookup, giving each open its own `TunFile`.
    dir.set_cacheable(false);
    dir
}
