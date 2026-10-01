//! Multi-mount protection refresh, separate from transaction writeback.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::os::BlockNotification;

impl Ext4Filesystem {
    pub(super) fn start_mmp_worker(&self) -> VfsResult<()> {
        self.mmp_worker
            .start("ext4-mmp", self.self_ref.clone(), run_mmp_worker)
    }
}

pub(super) fn mmp_identity_for_region(device_name: &str, region: BlockRegion) -> MmpIdentity {
    let region_suffix = alloc::format!("@{:x}", region.start_lba);
    let prefix_len = device_name
        .len()
        .min(32usize.saturating_sub(region_suffix.len()));
    let mut encoded_name = Vec::with_capacity(prefix_len + region_suffix.len());
    encoded_name.extend_from_slice(&device_name.as_bytes()[..prefix_len]);
    encoded_name.extend_from_slice(region_suffix.as_bytes());

    // ax-fs-ng has no global UTS identity. Keep the node field empty and record
    // the concrete device region instead of publishing a process-wide label.
    MmpIdentity::from_names(&[], &encoded_name)
}

fn run_mmp_worker(
    filesystem: Weak<Ext4Filesystem>,
    notification: Arc<dyn BlockNotification>,
    stopping: Arc<AtomicBool>,
) {
    let interval = match filesystem.upgrade() {
        Some(filesystem) => filesystem.lock().ext4.mmp_refresh_interval(),
        None => return,
    };
    let Some(mut interval) = interval else {
        return;
    };
    let mut last_refresh = crate::os::monotonic_time();

    // Linux also runs kmmpd without a delay when a crafted image contains a
    // zero update interval. Do not reinterpret zero as "disable ownership".
    while !stopping.load(Ordering::Acquire) {
        notification.wait_timeout(interval);
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let now = crate::os::monotonic_time();
        let elapsed = now.saturating_sub(last_refresh);
        let Some(filesystem) = filesystem.upgrade() else {
            break;
        };
        let refresh = filesystem.lock().ext4.refresh_mmp(elapsed);
        match refresh {
            Ok(Some(next_interval)) => {
                last_refresh = now;
                interval = next_interval;
            }
            Ok(None) => break,
            Err(error) => {
                log::error!("ext4 MMP refresh failed: {error}");
                break;
            }
        }
    }
}
