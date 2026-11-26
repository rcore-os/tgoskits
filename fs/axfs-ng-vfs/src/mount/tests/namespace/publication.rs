use std::{
    sync::{Mutex, mpsc},
    thread::{self, ThreadId},
    time::Duration,
};

use super::*;
use crate::mount::publication::test_hook;

#[derive(Debug, Default)]
struct Releases {
    total: AtomicUsize,
    topology: AtomicUsize,
    caller_writer: AtomicUsize,
}

#[derive(Debug, Default)]
struct PublicationCounts {
    filesystem: Arc<Releases>,
    lease: Arc<Releases>,
    attachment: Arc<Releases>,
    roots_prepared: AtomicUsize,
    leases_prepared: AtomicUsize,
    preparation_under_topology: AtomicUsize,
}

impl PublicationCounts {
    fn backend_releases(&self) -> [usize; 2] {
        [
            self.filesystem.total.load(Ordering::Relaxed),
            self.lease.total.load(Ordering::Relaxed),
        ]
    }

    fn assert_final(&self, attachments: usize) {
        assert_eq!(self.backend_releases(), [1, 1]);
        assert_eq!(self.attachment.total.load(Ordering::Relaxed), attachments);
        for resource in [&self.filesystem, &self.lease, &self.attachment] {
            assert_eq!(resource.topology.load(Ordering::Relaxed), 0);
            assert_eq!(resource.caller_writer.load(Ordering::Relaxed), 0);
        }
        assert_eq!(self.roots_prepared.load(Ordering::Relaxed), 1);
        assert_eq!(self.leases_prepared.load(Ordering::Relaxed), 1);
        assert_eq!(self.preparation_under_topology.load(Ordering::Relaxed), 0);
    }
}

#[derive(Clone, Debug)]
struct RecordingContext {
    counts: Arc<PublicationCounts>,
    writer: Arc<Mutex<()>>,
    caller: ThreadId,
}

impl RecordingContext {
    fn new() -> Self {
        Self {
            counts: Arc::new(PublicationCounts::default()),
            writer: Arc::new(Mutex::new(())),
            caller: thread::current().id(),
        }
    }

    fn probe(&self, releases: &Arc<Releases>) -> ReleaseProbe {
        ReleaseProbe {
            releases: releases.clone(),
            context: self.clone(),
        }
    }

    fn filesystem(&self) -> Filesystem {
        Filesystem::new(Arc::new_cyclic(|self_ref| RecordingFilesystem {
            self_ref: self_ref.clone(),
            lease: RawSpinLock::new(Weak::new()),
            context: self.clone(),
            _drop: self.probe(&self.counts.filesystem),
        }))
    }

    fn observe_preparation(&self) {
        self.counts.preparation_under_topology.fetch_add(
            usize::from(current_thread_owns_mount_topology_guard()),
            Ordering::Relaxed,
        );
    }
}

/// Drop never blocks, reenters a Core, or asserts. The remote Core may legally
/// release an attachment while this caller owns its separate writer.
#[derive(Debug)]
struct ReleaseProbe {
    releases: Arc<Releases>,
    context: RecordingContext,
}

impl Drop for ReleaseProbe {
    fn drop(&mut self) {
        self.releases.topology.fetch_add(
            usize::from(current_thread_owns_mount_topology_guard()),
            Ordering::Relaxed,
        );
        self.releases.caller_writer.fetch_add(
            usize::from(
                thread::current().id() == self.context.caller
                    && self.context.writer.try_lock().is_err(),
            ),
            Ordering::Relaxed,
        );
        // Counts publish no other data. Channels and join establish the
        // inter-thread order before any observations or assertions below.
        self.releases.total.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct RecordingFilesystem {
    self_ref: Weak<Self>,
    lease: RawSpinLock<Weak<RecordingLease>>,
    context: RecordingContext,
    _drop: ReleaseProbe,
}

#[derive(Debug)]
struct RecordingLease {
    _owner: Arc<RecordingFilesystem>,
    _drop: ReleaseProbe,
}

impl crate::FilesystemMountLease for RecordingLease {}

impl FilesystemOps for RecordingFilesystem {
    fn name(&self) -> &str {
        "publication-retirement-fixture"
    }

    fn root_dir(&self) -> DirEntry {
        self.context.observe_preparation();
        self.context
            .counts
            .roots_prepared
            .fetch_add(1, Ordering::Relaxed);
        let filesystem = self.self_ref.upgrade().expect("filesystem is alive");
        DirEntry::new_dir(
            |_| DirNode::new(Arc::new(RecordingDirectory(filesystem))),
            Reference::root(),
        )
    }

    fn mount_lease(&self) -> Option<Arc<dyn crate::FilesystemMountLease>> {
        self.context.observe_preparation();
        let mut slot = self.lease.lock();
        let lease = slot.upgrade().unwrap_or_else(|| {
            self.context
                .counts
                .leases_prepared
                .fetch_add(1, Ordering::Relaxed);
            let lease = Arc::new(RecordingLease {
                _owner: self.self_ref.upgrade().expect("filesystem is alive"),
                _drop: self.context.probe(&self.context.counts.lease),
            });
            *slot = Arc::downgrade(&lease);
            lease
        });
        Some(lease)
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::Unsupported)
    }
}

struct RecordingDirectory(Arc<RecordingFilesystem>);

impl NodeOps for RecordingDirectory {
    fn inode(&self) -> u64 {
        41
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(mock_metadata(41, NodeType::Directory, 0))
    }

    fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.0.as_ref()
    }

    fn sync(&self, _: bool) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for RecordingDirectory {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn read_dir(&self, _: DirectoryCursor, _: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Ok(0)
    }

    fn lookup(&self, _: &str) -> VfsResult<DirEntry> {
        Err(VfsError::NotFound)
    }
}

#[test]
fn publication_retires_success_and_rejection_owners_after_caller_writer() {
    publication_survives_remote_core_retirement();
    publication_rejects_retired_target_with_prepared_owners();
}

fn publication_survives_remote_core_retirement() {
    let context = RecordingContext::new();
    let p_anchor = Mountpoint::new_root(&mock_filesystem());
    let p = p_anchor.root_location().mount(&mock_filesystem()).unwrap();
    let p_namespace = adopt(&p_anchor);
    let q_anchor = Mountpoint::new_root(&mock_filesystem());
    let q = q_anchor
        .root_location()
        .bind_mount(&p.root_location(), false)
        .unwrap();
    let q_namespace = adopt(&q_anchor);
    p.set_shared();
    let c = q.root_location().mount(&context.filesystem()).unwrap();
    c.set_lifetime_guard(Arc::new(context.probe(&context.counts.attachment)));
    q.join_shared_group(&p);
    let q_weak = Arc::downgrade(&q);
    let c_weak = Arc::downgrade(&c);
    drop((c, q, q_anchor));

    let (go_tx, go_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        // A timeout only bounds a broken/missed handoff; it cannot pass.
        let triggered = go_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        drop(q_namespace);
        let _ = done_tx.send(triggered);
        triggered
    });
    let (sample_tx, sample_rx) = mpsc::channel();
    let hook_go = go_tx.clone();
    let hook_counts = context.counts.clone();
    let armed = test_hook::arm(&p, q_weak.clone(), move || {
        let outside_topology = !current_thread_owns_mount_topology_guard();
        let sent = hook_go.send(()).is_ok();
        let completed = done_rx.recv_timeout(Duration::from_secs(10)) == Ok(true);
        let _ = sample_tx.send((
            outside_topology,
            sent,
            completed,
            hook_counts.backend_releases(),
        ));
    });
    let retirement;
    let writer = context.writer.lock().unwrap();
    let (result, owners) = p.root_location().mount_retiring(&mock_filesystem());
    retirement = owners;
    let returned_releases = context.counts.backend_releases();
    let succeeded = result.is_ok();
    drop(result);
    // Also release and join the worker after an early error or a missed hook.
    // No assertion can leave a live worker or drop the carrier under writer.
    let _ = go_tx.send(());
    let joined = worker.join();
    let sample = sample_rx.try_recv();
    drop(writer);
    drop(retirement);
    drop(armed);
    drop((p_namespace, p, p_anchor));

    assert!(succeeded);
    assert!(
        matches!(joined, Ok(true)),
        "remote Core cleanup must complete"
    );
    assert_eq!(sample, Ok((true, true, true, [0, 0])));
    assert_eq!(returned_releases, [0, 0]);
    assert!(q_weak.upgrade().is_none());
    assert!(c_weak.upgrade().is_none());
    context.counts.assert_final(1);
}

fn publication_rejects_retired_target_with_prepared_owners() {
    let root = Mountpoint::new_root(&mock_filesystem());
    let target = root.root_location();
    drop(adopt(&root));
    drop(root);

    let context = RecordingContext::new();
    let filesystem = context.filesystem();
    let retirement;
    let writer = context.writer.lock().unwrap();
    let (result, owners) = target.mount_retiring(&filesystem);
    retirement = owners;
    drop(filesystem);
    let rejected = result.map(|_| ());
    let returned_releases = context.counts.backend_releases();
    drop(writer);
    drop(retirement);
    assert_eq!(rejected, Err(VfsError::InvalidInput));
    assert_eq!(returned_releases, [0, 0]);
    context.counts.assert_final(0);

    // Attach rejection must likewise pin its actual source, rather than
    // relying on the caller's Arc surviving inspection of the paired result.
    let context = RecordingContext::new();
    let source = Mountpoint::new_root(&context.filesystem());
    let source_weak = Arc::downgrade(&source);
    source.set_lifetime_guard(Arc::new(context.probe(&context.counts.attachment)));
    let retirement;
    let writer = context.writer.lock().unwrap();
    let (result, owners) = source.attach_detached_retiring(&target);
    retirement = owners;
    drop(source);
    let returned_releases = context.counts.backend_releases();
    drop(writer);
    drop(retirement);
    assert_eq!(result, Err(VfsError::InvalidInput));
    assert_eq!(returned_releases, [0, 0]);
    assert!(source_weak.upgrade().is_none());
    context.counts.assert_final(1);
}
