use super::*;
use crate::mount::propagation::clone_relation_hook::{self, UpgradeKind};

#[derive(Default)]
struct ReleaseObservations {
    released: AtomicUsize,
    under_topology: AtomicUsize,
    under_writer: AtomicUsize,
}

/// Observe the actual attached resource's final release without taking either
/// gate, running a Core destructor, or panicking during clone unwinding.
struct CounterpartAttachment {
    observations: Arc<ReleaseObservations>,
    writer: Arc<RawSpinLock<()>>,
}

impl Drop for CounterpartAttachment {
    fn drop(&mut self) {
        // All observations and assertions run on the same host-test thread.
        // Atomics provide interior mutability, not an inter-thread publication.
        self.observations.under_topology.fetch_add(
            usize::from(current_thread_owns_mount_topology_guard()),
            Ordering::Relaxed,
        );
        self.observations.under_writer.fetch_add(
            usize::from(self.writer.try_lock().is_none()),
            Ordering::Relaxed,
        );
        self.observations.released.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CloneReleaseOutcome {
    kind: UpgradeKind,
    upgrade_hits: usize,
    released_during_clone: usize,
    released_total: usize,
    under_topology: usize,
    under_writer: usize,
    mount_reclaimed: bool,
}

#[test]
fn namespace_clone_retires_external_propagation_owners_after_caller_gate() {
    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for kind in [UpgradeKind::SharedPeers, UpgradeKind::SlaveMasters] {
        let anchor = Mountpoint::new_root(&mock_filesystem());
        let member = anchor.root_location().mount(&mock_filesystem()).unwrap();
        let namespace = adopt(&anchor);
        let counterpart = Mountpoint::new_root(&mock_filesystem());
        counterpart.set_shared();
        member.join_shared_group(&counterpart);
        if kind == UpgradeKind::SlaveMasters {
            member.set_slave();
        }
        let counterpart_weak = Arc::downgrade(&counterpart);
        let observations = Arc::new(ReleaseObservations::default());
        let writer = Arc::new(RawSpinLock::new(()));
        counterpart.set_lifetime_guard(Arc::new(CounterpartAttachment {
            observations: observations.clone(),
            writer: writer.clone(),
        }));
        let upgrade_hits = Arc::new(AtomicUsize::new(0));
        // The registration takes the ONLY external strong counterpart owner.
        // Source and cloned mounts keep propagation edges as Weak, while the
        // actual clone map contains only anchor/member and their new nodes.
        let release = clone_relation_hook::arm(&member, kind, counterpart, upgrade_hits.clone());
        let writer_guard = writer.lock();
        let cloned = namespace.clone_namespace();
        let released_during_clone = observations.released.load(Ordering::Relaxed);
        let (created, retirement) = cloned.into_parts();
        drop(writer_guard);
        // A missed hook must not retain an external owner through assertions.
        // Its explicit hit count below also prevents a false green in that case.
        drop(release);
        drop((created, retirement, namespace));
        // Collect both real paths before asserting: the wrong shared case must
        // not prevent the slave/master case from executing or cleaning up.
        observed.push(CloneReleaseOutcome {
            kind,
            upgrade_hits: upgrade_hits.load(Ordering::Relaxed),
            released_during_clone,
            released_total: observations.released.load(Ordering::Relaxed),
            under_topology: observations.under_topology.load(Ordering::Relaxed),
            under_writer: observations.under_writer.load(Ordering::Relaxed),
            mount_reclaimed: counterpart_weak.upgrade().is_none(),
        });
        expected.push(CloneReleaseOutcome {
            kind,
            upgrade_hits: 1,
            released_during_clone: 0,
            released_total: 1,
            under_topology: 0,
            under_writer: 0,
            mount_reclaimed: true,
        });
    }
    assert_eq!(
        observed, expected,
        "namespace clone must retire upgraded tree-external owners after topology and caller gates"
    );
}
