//! Real dirty-inode eviction must classify an observation before pressure runs.

use axfs_ng_vfs::{
    MutationCredentials, NameState, NamespaceEffect, NamespaceOperationScope, NamespaceStart,
    NamespaceState, PreparedWrite,
};

use super::*;

struct BeforeFinish(Option<Box<dyn FnOnce()>>);

impl Drop for BeforeFinish {
    fn drop(&mut self) {
        self.0.take().unwrap()();
    }
}

pub(super) fn retry_observation_after_checkpoint_pressure() {
    for flush_result in [Ok(()), Err(BlockError::Io)] {
        let (filesystem, root, mount) = fixture();
        let parent = mount.root_location();
        let witness = parent.lookup_no_follow("witness").unwrap();
        let credentials = MutationCredentials::root();
        let write = PreparedWrite::acquire(&parent).unwrap();
        let prepared = parent.prepare_unlink("witness").unwrap();
        prepare_cold_target_checkpoint(&filesystem, &root);
        let root_number = inode_number(&root);
        let inode_owners = Arc::strong_count(&root);
        let filesystem_owners = Arc::strong_count(&filesystem);
        let scope = NamespaceOperationScope::acquire(&write).unwrap();
        let context = scope.context();
        let completion = match scope
            .unlink(&context, prepared, false, &credentials)
            .unwrap()
        {
            NamespaceStart::Complete(completion) => completion,
            NamespaceStart::Observed(_) => {
                panic!("cold target observation must fail before commit");
            }
        };
        // This is the backend handoff, before any pressure callback. Removing
        // finish()'s early classification must fail here, not at final unlink.
        assert_eq!(completion.state(), NamespaceState::Retry);
        assert!(completion.observation().is_none());
        assert!(filesystem.lock().ext4.writeback_checkpoint_pending());
        assert!(!filesystem.lock().staging);

        let failing = flush_result.is_err();
        let flushing = filesystem.clone();
        let probe = watch_flush(move || {
            assert!(flushing.inner.try_lock().is_some());
            drop(flushing.namespace.lookup(root_number).unwrap());
            assert_eq!(flushing.admission.active_count(), 1);
            flush_result
        });
        // The pressure object must transfer its real FS and both parent owners
        // to finish even after an observation failure or a pressure I/O error.
        // Weak probes add no owners; this runs after ordinary retirement and
        // all admission exits, immediately before scope invokes phase four.
        let weak_root = Arc::downgrade(&root);
        let weak_fs = Arc::downgrade(&filesystem);
        context
            .retired()
            .retire(BeforeFinish(Some(Box::new(move || {
                probe.finish();
                assert!(weak_root.strong_count() > inode_owners);
                assert!(weak_fs.strong_count() > filesystem_owners);
                assert_eq!(weak_fs.upgrade().unwrap().admission.active_count(), 0);
            }))));
        let outcome = scope
            .finish()
            .unwrap_or_else(|_| panic!("observation completion lost its actual owner"));
        assert_eq!(Arc::strong_count(&root), inode_owners);
        assert_eq!(Arc::strong_count(&filesystem), filesystem_owners);
        assert!(outcome.observation().is_none());

        // Name-only lookup needs the hot parent, not the cold target inode.
        // It remains readable after journal abort and proves no unlink reached
        // native name publication during either failed observation.
        let present = filesystem
            .lock()
            .ext4
            .lookup_child_number(root_number, rsext4::FileName::new(b"witness").unwrap())
            .unwrap();
        assert_eq!(present.map(|inode| inode.as_u64()), Some(witness.inode()));
        assert_eq!(witness.entry().name_state(), NameState::Linked);
        if failing {
            assert_eq!(outcome.state(), NamespaceState::Unchanged);
            assert_eq!(outcome.into_result(), Err(VfsError::Io));
            assert!(filesystem.lock().ext4.writeback_failure().is_some());
        } else {
            assert_eq!(outcome.state(), NamespaceState::Retry);
            assert_eq!(outcome.into_result(), Err(VfsError::WouldBlock));
            assert!(!filesystem.lock().ext4.writeback_checkpoint_pending());

            // A fresh request must observe and authorize again; the first
            // completion carried neither an observed target nor old authority.
            let prepared = parent.prepare_unlink("witness").unwrap();
            let scope = NamespaceOperationScope::acquire(&write).unwrap();
            let context = scope.context();
            let observed = match scope
                .unlink(&context, prepared, false, &credentials)
                .unwrap()
            {
                NamespaceStart::Observed(observed) => observed,
                NamespaceStart::Complete(_) => {
                    panic!("checkpoint progress must allow a fresh target observation");
                }
            };
            assert_eq!(observed.target().entry.inode(), witness.inode());
            let completion = observed.resume();
            assert_eq!(completion.state(), NamespaceState::Applied);
            assert!(matches!(
                completion.observation(),
                Some(NamespaceEffect::Unlink(entry)) if entry.entry.inode() == witness.inode()
            ));
            drop(completion);
            let outcome = scope
                .finish()
                .unwrap_or_else(|_| panic!("retried unlink lost its actual owner"));
            assert_eq!(outcome.state(), NamespaceState::Applied);
            assert_eq!(outcome.into_result(), Ok(()));
            assert_eq!(root.lookup("witness").unwrap_err(), VfsError::NotFound);
            assert_eq!(witness.metadata().unwrap().nlink, 0);
        }
        drop(write);
        assert_admission_released(&filesystem);
    }
}

fn prepare_cold_target_checkpoint(filesystem: &Ext4Filesystem, root: &Inode) {
    let numbers: Vec<_> = (0..rsext4::INODE_CACHE_MAX)
        .map(|index| {
            let entry = create_file(root, &alloc::format!("cache-{index}"));
            InodeNumber::new(entry.inode().try_into().unwrap()).unwrap()
        })
        .collect();
    filesystem.sync_to_disk().unwrap();
    let batch = {
        let mut state = filesystem.lock();
        // Touch and dirty a cache-capacity of distinct non-target inodes in
        // order, displacing the clean parent and target without cache hooks.
        for number in numbers {
            state
                .ext4
                .update_inode_metadata(
                    number,
                    rsext4::InodeMetadataUpdate {
                        uid: Some(1),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        // Reload only the parent. It displaces one dirty entry while mutation
        // is still admitted; all remaining eviction candidates are dirty.
        state.ext4.inode(inode_number(root)).unwrap();
        // Unlike prepare_sync_for_checkpoint, this seals only the published
        // prefix and leaves the other dirty inode records in the real cache.
        state
            .ext4
            .prepare_writeback_progress_for_checkpoint()
            .unwrap()
    };
    let mut receipt = batch.execute();
    let mut state = filesystem.lock();
    state.ext4.finish_sync(&mut receipt).unwrap();
    assert!(state.ext4.writeback_checkpoint_pending());
    assert!(!state.staging);
    // Parent reads still succeed, but lookup must load the cold witness and
    // flush a dirty victim through the checkpoint's closed mutation boundary.
    state.ext4.inode(inode_number(root)).unwrap();
    let error = state
        .ext4
        .lookup_child(
            inode_number(root),
            rsext4::FileName::new(b"witness").unwrap(),
        )
        .unwrap_err();
    assert!(error.requires_journal_progress());
}
