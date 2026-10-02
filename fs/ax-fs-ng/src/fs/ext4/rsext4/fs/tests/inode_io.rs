//! Hardlink mutations retain their shared inode owner through ext4 progress.

use core::cell::RefCell;

use axfs_ng_vfs::{FileRangeOperation, NodePermission, NodeType};

use super::*;

std::thread_local! {
    static PROBE: RefCell<Option<MutationProbe>> = const { RefCell::new(None) };
}

struct MutationProbe {
    inode: Arc<AccessGate>,
    observations: usize,
}

struct ProbeGuard;

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        PROBE.with_borrow_mut(Option::take);
    }
}

pub(super) fn inspect_ext4_lock() {
    PROBE.with_borrow_mut(|slot| {
        if let Some(probe) = slot {
            assert!(
                probe.inode.try_write().is_none(),
                "mutation lost the shared inode owner"
            );
            assert!(
                probe.inode.try_read().unwrap().is_none(),
                "mutation allowed a concurrent content reader"
            );
            probe.observations += 1;
        }
    });
}

#[test]
fn hardlink_write_append_truncate_and_hole_punch_share_inode_exclusion() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let input = root
        .create(
            "input",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    let alias = root.link("alias", &input).unwrap();
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    assert_eq!(alias.inode(), input.inode());
    let inode = filesystem.inode_access(number);
    PROBE.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(MutationProbe {
            inode,
            observations: 0,
        });
    });
    let _probe = ProbeGuard;
    let file = alias.entry().as_file().unwrap();

    file.write_at(&[0x73; 8192], 0).unwrap();
    require_observed_boundary();
    assert_eq!(file.append(b"tail"), Ok((4, 8196)));
    require_observed_boundary();
    file.set_len(8192).unwrap();
    require_observed_boundary();
    file.operate_range(0, 4096, FileRangeOperation::PunchHole)
        .unwrap();
    require_observed_boundary();
}

fn require_observed_boundary() {
    PROBE.with_borrow_mut(|slot| {
        let probe = slot.as_mut().unwrap();
        assert!(probe.observations > 0, "operation never entered ext4");
        assert!(
            probe.inode.try_write().is_some(),
            "operation leaked inode exclusion"
        );
        probe.observations = 0;
    });
}
