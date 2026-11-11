//! Component protocol probes use the real scope, dispatch and closed observation.

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use std::sync::{Mutex, MutexGuard};

use super::*;
use crate::{
    AuthorizedMutation, DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, Filesystem,
    FilesystemOps, LockedAttempt, MetadataRequest, MetadataUpdate, Mountpoint, MutationObservation,
    NamespaceDriver, NamespaceDriverRequest, NamespaceFinish, NamespaceMutation, NamespaceNode,
    NamespaceOperation, NamespacePressure, NamespacePressureResult, NodeOps, NodePermission,
    NodeType, Reference, RenameNode, StatFs,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Enter(usize),
    Exit(usize),
    Pressure(&'static str),
    Retire,
    Finish(&'static str),
    DropWork(&'static str),
    Release,
}

type Events = Arc<Mutex<Vec<Event>>>;

struct Admission<'a>(&'a ProbeFs);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.events.lock().unwrap().push(Event::Exit(self.0.id));
    }
}

struct Owner(Events);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.lock().unwrap().push(Event::Release);
    }
}

struct Retired(Events);
impl Drop for Retired {
    fn drop(&mut self) {
        self.0.lock().unwrap().push(Event::Retire);
    }
}

struct Work {
    events: Events,
    name: &'static str,
    pressure: VfsResult<()>,
    finish: VfsResult<()>,
    _owner: Arc<Owner>,
}

impl Drop for Work {
    fn drop(&mut self) {
        self.events.lock().unwrap().push(Event::DropWork(self.name));
    }
}

impl<T> NamespacePressure<T> for Work {
    fn pressure(self: Box<Self>, _: &mut NamespaceOutcome<T>) -> NamespacePressureResult {
        self.events.lock().unwrap().push(Event::Pressure(self.name));
        NamespacePressureResult::new(self.pressure, Some(self))
    }
}

impl NamespaceFinish for Work {
    fn finish(self: Box<Self>) -> VfsResult<()> {
        self.events.lock().unwrap().push(Event::Finish(self.name));
        self.finish
    }
}

struct ProbeFs {
    this: Weak<Self>,
    id: usize,
    roots: Vec<Location>,
    events: Events,
    owner: Arc<Owner>,
    pending: Mutex<Vec<PreparedUnlink>>,
    action: AtomicUsize,
    gate: Mutex<()>,
    root: Mutex<NamespaceOutcome<Metadata>>,
    root_work: bool,
    phase_errors: bool,
}

impl ProbeFs {
    fn work(&self, name: &'static str) -> Work {
        Work {
            events: self.events.clone(),
            name,
            pressure: if self.phase_errors && name == "metadata" {
                Err(VfsError::NoMemory)
            } else {
                Ok(())
            },
            finish: if self.phase_errors {
                Err(VfsError::Io)
            } else {
                Ok(())
            },
            _owner: self.owner.clone(),
        }
    }

    fn entry(&self, reference: Reference) -> DirEntry {
        let node = Arc::new(ProbeNode(self.this.upgrade().unwrap()));
        DirEntry::new_dir(|_| DirNode::new(node), reference)
    }
}

impl FilesystemOps for ProbeFs {
    fn name(&self) -> &str {
        "completion-protocol-probe"
    }
    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::Unsupported)
    }
    fn root_dir(&self) -> DirEntry {
        self.entry(Reference::root())
    }
    fn stack_depth(&self) -> u32 {
        u32::from(!self.roots.is_empty())
    }
    fn write_backing_roots(&self) -> &[Location] {
        &self.roots
    }
    fn enter_namespace(&self) -> VfsResult<Box<dyn NamespaceOperation + '_>> {
        self.events.lock().unwrap().push(Event::Enter(self.id));
        Ok(Box::new(Admission(self)))
    }
    fn query_namespace(
        &self,
        _: &NamespaceAdmission<'_>,
        _: NamespaceQuery<'_>,
        _: &MutationRetire,
    ) -> VfsResult<NamespaceRead> {
        Ok(NamespaceRead::Metadata(snapshot()))
    }
    fn begin_namespace<'a>(&'a self, request: NamespaceDriverRequest<'a>) -> NamespaceStart<'a> {
        let guard = self.gate.lock().unwrap();
        let parent = request.command().parents().0.clone();
        let target = self.entry(Reference::new(Some(parent), "victim".into()));
        request.observe(Box::new(Driver {
            filesystem: self,
            _guard: guard,
            target,
        }))
    }
}

struct ProbeNode(Arc<ProbeFs>);
impl NodeOps for ProbeNode {
    fn inode(&self) -> u64 {
        1
    }
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(snapshot())
    }
    fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }
    fn sync(&self, _: bool) -> VfsResult<()> {
        Ok(())
    }
    fn filesystem(&self) -> &dyn FilesystemOps {
        self.0.as_ref()
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
        if let Some(backing) = self.0.roots.first() {
            let observed = request
                .context()
                .change_metadata(
                    backing,
                    MetadataChange::Chown {
                        uid: None,
                        gid: None,
                    },
                    &MutationCredentials::root(),
                )
                .unwrap()
                .unwrap();
            // This probe treats an expected backing rejection as a completed
            // branch, then returns its independently selected logical prefix.
            // The processor must not promote that old error into a phase error.
            assert_eq!(observed.state(), NamespaceState::Unchanged);
            assert_eq!(observed.result(), Err(VfsError::AlreadyExists));
        }
        let pending = core::mem::take(&mut *self.0.pending.lock().unwrap());
        for prepared in pending {
            let start = request
                .context()
                .unlink(prepared, true, &MutationCredentials::root())
                .unwrap();
            match (self.0.action.load(Ordering::Relaxed), start) {
                (1, NamespaceStart::Observed(observed)) => {
                    let cancelled = observed.cancel();
                    assert_eq!(cancelled.result(), Ok(()));
                }
                (2, NamespaceStart::Observed(observed)) => drop(observed),
                (0, start) => {
                    let observed = start.resume();
                    assert_eq!(observed.state(), NamespaceState::Applied);
                }
                _ => panic!("expected a real closed observation"),
            }
            let registry = request.context().children.borrow();
            assert!(
                registry.children.capacity() >= registry.children.len() + registry.reservations,
                "recursive return must leave every outer reservation usable without allocation"
            );
        }
        let name = if self.0.roots.is_empty() {
            "metadata"
        } else {
            "root"
        };
        NamespaceCompletion::new(
            self.0.root.lock().unwrap().clone(),
            (self.0.roots.is_empty() || self.0.root_work)
                .then(|| Box::new(self.0.work(name)) as Box<dyn NamespacePressure<Metadata>>),
        )
    }
}

impl DirNodeOps for ProbeNode {
    fn namespace_domain(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    fn read_dir(&self, _: DirectoryCursor, _: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Ok(0)
    }
    fn lookup(&self, _: &str) -> VfsResult<DirEntry> {
        Err(VfsError::NotFound)
    }
    fn is_cacheable(&self) -> bool {
        false
    }
}

struct Driver<'a> {
    filesystem: &'a ProbeFs,
    _guard: MutexGuard<'a, ()>,
    target: DirEntry,
}
impl LockedAttempt for Driver<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        Ok((snapshot(), snapshot()))
    }
    fn observe(
        &mut self,
        _: &NamespaceMutation<'_>,
        _: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        Ok(MutationObservation::Unlink(RenameNode {
            entry: self.target.clone(),
            metadata: snapshot(),
            canonical_name: "victim".into(),
            cacheable: false,
            empty: true,
        }))
    }
    fn is_ancestor(&mut self, _: &NamespaceNode, _: u64) -> VfsResult<bool> {
        Ok(false)
    }
    fn commit(
        &mut self,
        _: &AuthorizedMutation<'_>,
        _: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        NamespaceOutcome::applied(
            NamespaceEffect::Unlink(crate::NamespaceEntry {
                entry: self.target.clone(),
                canonical_name: "victim".into(),
                cacheable: false,
            }),
            Ok(()),
        )
    }
}
impl NamespaceDriver for Driver<'_> {
    fn finish(self: Box<Self>, outcome: NamespaceOutcome<NamespaceEffect>) -> NamespaceCompletion {
        let work = self.filesystem.work("namespace");
        drop(self);
        NamespaceCompletion::new(outcome, Some(Box::new(work)))
    }
}

fn snapshot() -> Metadata {
    Metadata {
        inode: 1,
        device: 0,
        nlink: 1,
        mode: NodePermission::from_bits_truncate(0o777),
        node_type: NodeType::Directory,
        uid: 0,
        gid: 0,
        size: 0,
        block_size: 4096,
        blocks: 0,
        rdev: crate::DeviceId::default(),
        atime: Duration::ZERO,
        mtime: Duration::ZERO,
        ctime: Duration::ZERO,
    }
}

fn filesystem(
    id: usize,
    roots: Vec<Location>,
    events: &Events,
    owner: &Arc<Owner>,
    root_work: bool,
    phase_errors: bool,
) -> (Arc<ProbeFs>, Location) {
    let layered = !roots.is_empty();
    let filesystem = Arc::new_cyclic(|this| ProbeFs {
        this: this.clone(),
        id,
        roots,
        events: events.clone(),
        owner: owner.clone(),
        pending: Mutex::new(Vec::new()),
        action: AtomicUsize::new(0),
        gate: Mutex::new(()),
        root: Mutex::new(if !layered {
            NamespaceOutcome::unchanged_observed(snapshot(), Err(VfsError::AlreadyExists))
        } else {
            NamespaceOutcome::applied(snapshot(), Ok(()))
        }),
        root_work,
        phase_errors,
    });
    let mount = Mountpoint::new_root(&Filesystem::new(filesystem.clone()));
    (filesystem, mount.root_location())
}

/// Called by the existing completion test; no second test or state-machine fake.
pub(crate) fn exercise_scope() {
    for (root_work, original, state, expected) in [
        (true, Ok(()), NamespaceState::Applied, VfsError::NoMemory),
        (false, Ok(()), NamespaceState::Applied, VfsError::NoMemory),
        (
            true,
            Err(VfsError::PermissionDenied),
            NamespaceState::Applied,
            VfsError::PermissionDenied,
        ),
        (
            true,
            Err(VfsError::WouldBlock),
            NamespaceState::Applied,
            VfsError::WouldBlock,
        ),
        (
            true,
            Err(VfsError::WouldBlock),
            NamespaceState::Retry,
            VfsError::NoMemory,
        ),
    ] {
        let events = Events::default();
        let owner = Arc::new(Owner(events.clone()));
        let weak_owner = Arc::downgrade(&owner);
        let (native, backing) = filesystem(1, vec![], &events, &owner, true, true);
        let weak_native = Arc::downgrade(&native);
        // More than Vec's minimum capacity makes an unaccounted outer slot fail
        // deterministically while the metadata child reservation is outstanding.
        *native.pending.lock().unwrap() = (0..12)
            .map(|_| backing.prepare_unlink("victim").unwrap())
            .collect();
        let (logical, root) =
            filesystem(0, vec![backing.clone()], &events, &owner, root_work, true);
        let weak_logical = Arc::downgrade(&logical);
        *logical.root.lock().unwrap() = if state == NamespaceState::Retry {
            NamespaceOutcome::retry()
        } else {
            NamespaceOutcome::applied(snapshot(), original)
        };
        let write = PreparedWrite::acquire(&root).unwrap();
        let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
        let context = scope.context();
        context.retired().retire(Retired(events.clone()));
        let observed = scope
            .change_metadata(
                &context,
                &root,
                MetadataChange::Chown {
                    uid: None,
                    gid: None,
                },
                &MutationCredentials::root(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(observed.state(), state);
        drop(observed);
        let outcome = scope
            .finish()
            .unwrap_or_else(|_| panic!("actual owner mismatch"));
        assert_eq!(
            outcome.state(),
            if state == NamespaceState::Retry {
                NamespaceState::Unchanged
            } else {
                state
            }
        );
        assert_eq!(
            outcome.observation().map(|value| value.inode),
            (state != NamespaceState::Retry).then_some(1)
        );
        assert_eq!(outcome.into_result(), Err(expected));
        let trace = events.lock().unwrap();
        let first_exit = trace
            .iter()
            .position(|event| matches!(event, Event::Exit(_)))
            .unwrap();
        let retire = trace
            .iter()
            .position(|event| *event == Event::Retire)
            .unwrap();
        let first_finish = trace
            .iter()
            .position(|event| matches!(event, Event::Finish(_)))
            .unwrap();
        assert_eq!(
            trace
                .iter()
                .filter(|event| matches!(event, Event::Enter(_)))
                .count(),
            2
        );
        assert_eq!(
            trace
                .iter()
                .filter(|event| matches!(event, Event::Exit(_)))
                .count(),
            2
        );
        assert!(
            trace[..first_exit]
                .iter()
                .all(|event| !matches!(event, Event::Finish(_)))
        );
        assert!(
            trace[first_exit..]
                .iter()
                .all(|event| !matches!(event, Event::Pressure(_)))
        );
        assert!(first_exit < retire && retire < first_finish);
        let mut expected_names = vec!["namespace"; 12];
        expected_names.push("metadata");
        if root_work {
            expected_names.push("root");
        }
        let pressures: Vec<_> = trace
            .iter()
            .filter_map(|event| match event {
                Event::Pressure(name) => Some(*name),
                _ => None,
            })
            .collect();
        let finishes: Vec<_> = trace
            .iter()
            .filter_map(|event| match event {
                Event::Finish(name) => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(pressures, expected_names);
        assert_eq!(finishes, expected_names);
        drop(trace);
        drop(write);
        drop(root);
        drop(logical);
        drop(backing);
        drop(native);
        drop(owner);
        assert!(weak_owner.upgrade().is_none());
        assert!(weak_native.upgrade().is_none());
        assert!(weak_logical.upgrade().is_none());
    }
    exercise_incomplete_and_preflight();
}

fn exercise_incomplete_and_preflight() {
    for (action, uncertain) in [(1, false), (2, false), (2, true)] {
        let events = Events::default();
        let owner = Arc::new(Owner(events.clone()));
        let weak_owner = Arc::downgrade(&owner);
        let (native, backing) = filesystem(1, vec![], &events, &owner, true, false);
        let weak_native = Arc::downgrade(&native);
        native.action.store(action, Ordering::Relaxed);
        native
            .pending
            .lock()
            .unwrap()
            .push(backing.prepare_unlink("victim").unwrap());
        let (logical, root) = filesystem(0, vec![backing.clone()], &events, &owner, true, false);
        let weak_logical = Arc::downgrade(&logical);
        let expected_state = if uncertain {
            *logical.root.lock().unwrap() =
                NamespaceOutcome::uncertain(Some(snapshot()), Err(VfsError::Io));
            NamespaceState::Uncertain
        } else {
            NamespaceState::Applied
        };
        let write = PreparedWrite::acquire(&root).unwrap();
        let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
        let context = scope.context();
        // Unstarted reservation cancellation is not an incomplete operation.
        drop(Reservation::child(context.children).unwrap());
        let observation = scope
            .change_metadata(
                &context,
                &root,
                MetadataChange::Chown {
                    uid: None,
                    gid: None,
                },
                &MutationCredentials::root(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(observation.state(), expected_state);
        assert!(
            native.gate.try_lock().is_ok(),
            "observation released the real gate"
        );
        assert_eq!(scope.state().children.borrow().reservations, 0);
        drop(observation);
        let outcome = scope
            .finish()
            .unwrap_or_else(|_| panic!("actual owner mismatch"));
        assert_eq!(outcome.state(), expected_state);
        assert!(outcome.observation().is_some());
        assert_eq!(
            outcome.into_result(),
            if uncertain {
                Err(VfsError::Io)
            } else if action == 1 {
                Ok(())
            } else {
                Err(VfsError::BadState)
            }
        );
        drop(write);
        drop(root);
        drop(logical);
        drop(backing);
        drop(native);
        drop(owner);
        assert!(weak_owner.upgrade().is_none());
        assert!(weak_native.upgrade().is_none());
        assert!(weak_logical.upgrade().is_none());
    }

    let events = Events::default();
    let owner = Arc::new(Owner(events.clone()));
    let weak_owner = Arc::downgrade(&owner);
    let (native, root) = filesystem(1, vec![], &events, &owner, true, false);
    let (foreign, foreign_root) = filesystem(2, vec![], &events, &owner, true, false);
    let write = PreparedWrite::acquire(&root).unwrap();
    let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
    {
        let context = scope.context();
        context.retired().retire(Retired(events.clone()));
        // Root and both typed child sets must be checked before any pressure.
        *scope.state().root.borrow_mut() = Some(
            NamespaceCompletion::new(
                NamespaceOutcome::applied(snapshot(), Ok(())),
                Some(Box::new(native.work("root"))),
            )
            .bind(native.as_ref()),
        );
        let _ = Reservation::child(context.children).unwrap().namespace(
            NamespaceCompletion::new(
                NamespaceOutcome::unchanged(Ok(())),
                Some(Box::new(native.work("namespace"))),
            )
            .bind(native.as_ref()),
        );
        let _ = Reservation::child(context.children).unwrap().metadata(
            NamespaceCompletion::new(
                NamespaceOutcome::applied(snapshot(), Ok(())),
                Some(Box::new(foreign.work("metadata"))),
            )
            .bind(foreign.as_ref()),
        );
    }
    let scope = match scope.finish() {
        Ok(_) => panic!("foreign child must preserve the entire scope"),
        Err(scope) => scope,
    };
    assert_eq!(scope.state().children.borrow().children.len(), 2);
    assert!(scope.state().root.borrow().is_some());
    assert_eq!(*events.lock().unwrap(), vec![Event::Enter(1)]);
    {
        let context = scope.context();
        assert!(matches!(
            context
                .query(NamespaceQuery::Metadata(root.entry()))
                .unwrap(),
            NamespaceRead::Metadata(_)
        ));
    }
    drop(scope);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            Event::Enter(1),
            Event::Exit(1),
            Event::Retire,
            Event::DropWork("namespace"),
            Event::DropWork("metadata"),
            Event::DropWork("root"),
        ]
    );
    drop(write);
    drop(root);
    drop(native);
    drop(foreign_root);
    drop(foreign);
    drop(owner);
    assert!(weak_owner.upgrade().is_none());

    let events = Events::default();
    let owner = Arc::new(Owner(events.clone()));
    let weak_owner = Arc::downgrade(&owner);
    let (native, root) = filesystem(1, vec![], &events, &owner, true, false);
    let weak_native = Arc::downgrade(&native);
    let write = PreparedWrite::acquire(&root).unwrap();
    let scope = NamespaceOperationScope::acquire(&write).unwrap();
    {
        let context = scope.context();
        let start = scope
            .unlink(
                &context,
                root.prepare_unlink("victim").unwrap(),
                true,
                &MutationCredentials::root(),
            )
            .unwrap();
        assert!(matches!(start, NamespaceStart::Observed(_)));
        drop(start);
    }
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("actual owner mismatch"));
    assert_eq!(outcome.state(), NamespaceState::Uncertain);
    assert!(outcome.observation().is_none());
    assert_eq!(outcome.into_result(), Err(VfsError::BadState));
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .all(|event| !matches!(event, Event::Pressure(_) | Event::Finish(_)))
    );
    drop(write);
    drop(root);
    drop(native);
    drop(owner);
    assert!(weak_owner.upgrade().is_none());
    assert!(weak_native.upgrade().is_none());
}
