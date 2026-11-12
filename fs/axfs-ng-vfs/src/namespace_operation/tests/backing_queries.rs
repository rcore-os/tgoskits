//! Real request/dispatch probes, not an Overlay or an IRQ runtime model.

use super::*;
use crate::{BackingQueryContext, BackingQueryRequest};

pub(super) struct QueryProbe {
    pub depth: u32,
    pub reads: Vec<Location>,
    rejections: Mutex<Vec<(Location, VfsError)>>,
    response: Mutex<Option<VfsResult<NamespaceRead>>>,
    pub hooks: AtomicUsize,
    temporary: Mutex<Option<DirEntry>>,
    cases: Mutex<Vec<ReadCase>>,
}

struct ReadCase {
    receiver: Weak<ProbeFs>,
    kind: CaseKind,
    response: VfsResult<NamespaceRead>,
    temporary: Option<DirEntry>,
    error: VfsError,
}

enum CaseKind {
    Metadata,
    Lookup,
    Xattr,
}

impl QueryProbe {
    fn new(depth: u32, reads: Vec<Location>) -> Self {
        Self {
            depth,
            reads,
            rejections: Mutex::new(Vec::new()),
            response: Mutex::new(None),
            hooks: AtomicUsize::new(0),
            temporary: Mutex::new(None),
            cases: Mutex::new(Vec::new()),
        }
    }

    fn reject_targets(&self, view: &BackingQueryContext<'_>) {
        for (target, expected) in core::mem::take(&mut *self.rejections.lock().unwrap()) {
            let backend = target.entry().downcast::<ProbeNode>().unwrap().0.clone();
            let calls = backend
                .queries
                .as_ref()
                .unwrap()
                .hooks
                .load(Ordering::Relaxed);
            // The valid name detects accidental dispatch; the invalid name
            // additionally proves admission/relationship error precedence.
            for name in ["candidate", "bad/name"] {
                assert!(matches!(
                    view.lookup_backing(&target, name),
                    Err(error) if error == expected
                ));
            }
            assert_eq!(
                backend
                    .queries
                    .as_ref()
                    .unwrap()
                    .hooks
                    .load(Ordering::Relaxed),
                calls,
                "rejected relationship entered a destination hook"
            );
        }
    }

    pub fn exercise(&self, view: BackingQueryContext<'_>) {
        self.reject_targets(&view);
        let backing = &self.reads[0];
        assert!(matches!(
            view.query(NamespaceQuery::Metadata(backing.entry())),
            Ok(NamespaceRead::Metadata(_))
        ));
        assert!(matches!(
            view.query(NamespaceQuery::Lookup {
                parent: backing.entry(),
                name: "missing"
            }),
            Ok(NamespaceRead::Entry(None))
        ));
        assert!(matches!(
            view.lookup_backing(backing, "missing"),
            Err(VfsError::NotFound)
        ));
        let backend = backing.entry().downcast::<ProbeNode>().unwrap().0.clone();
        let selected = backend.root_dir();
        *backend.queries.as_ref().unwrap().response.lock().unwrap() =
            Some(Ok(NamespaceRead::Entry(Some(selected.clone()))));
        let found = view.lookup_backing(backing, "present").unwrap();
        assert!(Arc::ptr_eq(found.mountpoint(), backing.mountpoint()));
        assert!(found.entry().ptr_eq(&selected));
        let calls = backend
            .queries
            .as_ref()
            .unwrap()
            .hooks
            .load(Ordering::Relaxed);
        assert!(matches!(
            view.lookup_backing(backing, "bad/name"),
            Err(VfsError::InvalidInput)
        ));
        assert_eq!(
            backend
                .queries
                .as_ref()
                .unwrap()
                .hooks
                .load(Ordering::Relaxed),
            calls
        );
        for case in core::mem::take(&mut *self.cases.lock().unwrap()) {
            let receiver = case.receiver.upgrade().unwrap();
            let probe = receiver.queries.as_ref().unwrap();
            *probe.response.lock().unwrap() = Some(case.response);
            *probe.temporary.lock().unwrap() = case.temporary;
            let query = match case.kind {
                CaseKind::Metadata => NamespaceQuery::Metadata(backing.entry()),
                CaseKind::Lookup => NamespaceQuery::Lookup {
                    parent: backing.entry(),
                    name: "candidate",
                },
                CaseKind::Xattr => NamespaceQuery::Xattr {
                    entry: backing.entry(),
                    name: b"user.test",
                },
            };
            assert!(matches!(view.query(query), Err(error) if error == case.error));
        }
    }

    pub fn read(
        &self,
        query: NamespaceQuery<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<NamespaceRead> {
        // The backend, not the VFS, owns temporary owners on an Err path.
        if let Some(temporary) = self.temporary.lock().unwrap().take() {
            retired.retire(temporary);
        }
        if let Some(response) = self.response.lock().unwrap().take() {
            return response;
        }
        Ok(match query {
            NamespaceQuery::Metadata(_) => NamespaceRead::Metadata(snapshot()),
            NamespaceQuery::Lookup { .. } => NamespaceRead::Entry(None),
            NamespaceQuery::Xattr { .. } => NamespaceRead::Xattr(vec![7]),
        })
    }
}

pub(super) fn dispatch(
    filesystem: &ProbeFs,
    request: BackingQueryRequest<'_, '_>,
) -> VfsResult<NamespaceRead> {
    // This must precede the destination gate and the new receiver-bound view.
    request.admission().check_owner(filesystem)?;
    let Some(probe) = &filesystem.queries else {
        return request.execute_native();
    };
    probe.hooks.fetch_add(1, Ordering::Relaxed);
    if probe.reads.is_empty() {
        let view = request.backing_queries();
        let result = request.execute_native();
        // The view survives consumption of the request, not its query borrow.
        probe.reject_targets(&view);
        return result;
    }
    let _guard = filesystem.gate.lock().unwrap();
    let view = request.backing_queries();
    probe.reject_targets(&view);
    let next = &probe.reads[0];
    // This hop must use B's dependency edge, not the original A's view.
    assert!(matches!(
        view.query(NamespaceQuery::Metadata(next.entry()))?,
        NamespaceRead::Metadata(_)
    ));
    probe.read(
        match request.query() {
            NamespaceQuery::Metadata(entry) => NamespaceQuery::Metadata(entry),
            NamespaceQuery::Lookup { parent, name } => NamespaceQuery::Lookup { parent, name },
            NamespaceQuery::Xattr { entry, name } => NamespaceQuery::Xattr { entry, name },
        },
        request.retired(),
    )
}

fn layer(
    id: usize,
    depth: u32,
    reads: Vec<Location>,
    events: &Events,
    owner: &Arc<Owner>,
) -> (Arc<ProbeFs>, Location) {
    let filesystem = Arc::new_cyclic(|this| ProbeFs {
        this: this.clone(),
        id,
        roots: vec![],
        events: events.clone(),
        owner: owner.clone(),
        pending: Mutex::new(vec![]),
        action: AtomicUsize::new(0),
        gate: Mutex::new(()),
        root: Mutex::new(NamespaceOutcome::unchanged_observed(snapshot(), Ok(()))),
        root_work: false,
        phase_errors: false,
        backend_calls: AtomicUsize::new(0),
        queries: Some(QueryProbe::new(depth, reads)),
    });
    let root = Mountpoint::new_root(&Filesystem::new(filesystem.clone())).root_location();
    (filesystem, root)
}

struct LastOwner {
    events: Events,
    gates: Vec<Weak<ProbeFs>>,
    releases: Arc<Mutex<Vec<(bool, usize, usize)>>>,
}

impl Drop for LastOwner {
    fn drop(&mut self) {
        // Record rather than assert in Drop, so a negative implementation
        // remains an ordinary test failure instead of a double-panic abort.
        let under_gate = self.gates.iter().any(|gate| {
            gate.upgrade()
                .is_some_and(|filesystem| filesystem.gate.try_lock().is_err())
        });
        let events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let enters = events
            .iter()
            .filter(|event| matches!(event, Event::Enter(_)))
            .count();
        let exits = events
            .iter()
            .filter(|event| matches!(event, Event::Exit(_)))
            .count();
        self.releases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((under_gate, enters, exits));
    }
}

#[test]
fn request_bound_queries_relay_direct_readonly_edges_and_retire_all_candidates() {
    let events = Events::default();
    let owner = Arc::new(Owner(events.clone()));
    let (c, c_root) = layer(2, 0, vec![], &events, &owner);
    c_root.mountpoint().set_readonly(true).unwrap();
    let (b, b_root) = layer(1, 1, vec![c_root.clone()], &events, &owner);
    let (sibling, sibling_root) = layer(3, 1, vec![], &events, &owner);
    let (extra, extra_root) = layer(4, 0, vec![], &events, &owner);
    let (missing, missing_root) = layer(5, 0, vec![], &events, &owner);
    let (a, a_root) = layer(
        0,
        2,
        vec![b_root.clone(), sibling_root.clone(), extra_root.clone()],
        &events,
        &owner,
    );
    // A real bind mount of B is not a descent from B to a different FS.
    let alias_target = Location::new(
        a_root.mountpoint().clone(),
        a.entry(Reference::new(Some(a_root.entry().clone()), "alias".into())),
    );
    let alias = alias_target
        .bind_mount(&b_root, false)
        .unwrap()
        .root_location();
    alias.detach_mount().unwrap();
    let gates = vec![Arc::downgrade(&a), Arc::downgrade(&b), Arc::downgrade(&c)];
    let releases = Arc::new(Mutex::new(Vec::new()));
    let candidate = |filesystem: &ProbeFs| {
        let entry = filesystem.root_dir();
        entry.user_data().insert(LastOwner {
            events: events.clone(),
            gates: gates.clone(),
            releases: releases.clone(),
        });
        entry
    };
    let rejected_parent = Location::new(missing_root.mountpoint().clone(), candidate(&missing));
    *a.queries.as_ref().unwrap().rejections.lock().unwrap() = vec![
        (c_root.clone(), VfsError::BadState), // admitted but indirect
        (a_root.clone(), VfsError::BadState),
        (rejected_parent, VfsError::CrossesDevices),
    ];
    *b.queries.as_ref().unwrap().rejections.lock().unwrap() = vec![
        // A missing direct-edge check reaches this unlocked lower receiver,
        // failing before a mutant could reenter its own still-held gate.
        (extra_root.clone(), VfsError::BadState),
        (b_root.clone(), VfsError::BadState),
        (alias, VfsError::BadState),
        (a_root.clone(), VfsError::BadState),
        (sibling_root.clone(), VfsError::BadState),
        (missing_root.clone(), VfsError::CrossesDevices),
    ];
    *a.queries.as_ref().unwrap().cases.lock().unwrap() = vec![
        ReadCase {
            receiver: Arc::downgrade(&b),
            kind: CaseKind::Metadata,
            response: Ok(NamespaceRead::Entry(Some(candidate(&c)))),
            temporary: None,
            error: VfsError::BadState,
        },
        ReadCase {
            receiver: Arc::downgrade(&b),
            kind: CaseKind::Lookup,
            response: Ok(NamespaceRead::Entry(Some(candidate(&c)))),
            temporary: None,
            error: VfsError::CrossesDevices,
        },
        ReadCase {
            receiver: Arc::downgrade(&b),
            kind: CaseKind::Metadata,
            response: Err(VfsError::Io),
            temporary: Some(candidate(&b)),
            error: VfsError::Io,
        },
        ReadCase {
            receiver: Arc::downgrade(&c),
            kind: CaseKind::Metadata,
            response: Ok(NamespaceRead::Entry(Some(candidate(&c)))),
            temporary: None,
            error: VfsError::BadState,
        },
        ReadCase {
            receiver: Arc::downgrade(&b),
            kind: CaseKind::Xattr,
            response: Ok(NamespaceRead::Entry(Some(candidate(&c)))),
            temporary: None,
            error: VfsError::BadState,
        },
        ReadCase {
            receiver: Arc::downgrade(&b),
            kind: CaseKind::Lookup,
            response: Ok(NamespaceRead::Metadata(snapshot())),
            temporary: None,
            error: VfsError::BadState,
        },
    ];
    let write = PreparedWrite::acquire(&a_root).unwrap();
    for target in [&b_root, &c_root, &extra_root] {
        assert!(matches!(
            write.write_for(target),
            Err(VfsError::CrossesDevices)
        ));
    }
    let scope = NamespaceOperationScope::acquire(&write).unwrap();
    {
        let context = scope.context();
        // The real driver stores the 'op view, then request.observe consumes the
        // request and invokes that driver while A's actual gate remains held.
        let observed = scope
            .create(
                &context,
                &a_root,
                "victim",
                creation_attributes(),
                crate::CreateIntent::Open(crate::ExistingOpen {
                    access: NodePermission::OTHER_READ,
                }),
                &MutationCredentials::root(),
            )
            .unwrap();
        assert_eq!(observed.result(), Ok(()));
        // MetadataRequest's independent factory follows the same real chain.
        assert_eq!(
            context
                .change_metadata(
                    &a_root,
                    MetadataChange::Chmod(NodePermission::from_bits_truncate(0o755)),
                    &MutationCredentials::root(),
                )
                .unwrap()
                .unwrap()
                .result(),
            Ok(())
        );
        assert!(b.queries.as_ref().unwrap().hooks.load(Ordering::Relaxed) >= 6);
        assert!(c.queries.as_ref().unwrap().hooks.load(Ordering::Relaxed) >= 6);
        assert_eq!(
            sibling
                .queries
                .as_ref()
                .unwrap()
                .hooks
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            extra
                .queries
                .as_ref()
                .unwrap()
                .hooks
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            missing
                .queries
                .as_ref()
                .unwrap()
                .hooks
                .load(Ordering::Relaxed),
            0
        );

        // New full-context first hops also normalize responses. Each candidate
        // is a sole uncached owner after the hook hands it back to production.
        let probe = b.queries.as_ref().unwrap();
        for query in [
            NamespaceQuery::Metadata(b_root.entry()),
            NamespaceQuery::Xattr {
                entry: b_root.entry(),
                name: b"user.test",
            },
        ] {
            *probe.response.lock().unwrap() = Some(Ok(NamespaceRead::Entry(Some(candidate(&c)))));
            assert!(matches!(context.query(query), Err(VfsError::BadState)));
        }
        *probe.response.lock().unwrap() = Some(Ok(NamespaceRead::Xattr(vec![])));
        assert!(matches!(
            context.lookup_backing(&b_root, "wrong"),
            Err(VfsError::BadState)
        ));
        *probe.response.lock().unwrap() = Some(Ok(NamespaceRead::Entry(Some(candidate(&c)))));
        assert!(matches!(
            context.lookup_backing(&b_root, "foreign"),
            Err(VfsError::CrossesDevices)
        ));
        *probe.temporary.lock().unwrap() = Some(candidate(&b));
        *probe.response.lock().unwrap() = Some(Err(VfsError::Io));
        assert!(matches!(
            context.query(NamespaceQuery::Metadata(b_root.entry())),
            Err(VfsError::Io)
        ));
        assert!(matches!(
            context.query(NamespaceQuery::Lookup {
                parent: c_root.entry(),
                name: "missing"
            }),
            Ok(NamespaceRead::Entry(None))
        ));
        assert!(matches!(
            context.lookup_backing(&c_root, "missing"),
            Err(VfsError::NotFound)
        ));
        assert!(
            matches!(context.query(NamespaceQuery::Xattr { entry: c_root.entry(), name: b"user.test" }), Ok(NamespaceRead::Xattr(value)) if value == vec![7])
        );
        assert!(releases.lock().unwrap().is_empty());
    }
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("real query operation owner"));
    assert_eq!(outcome.into_result(), Ok(()));
    let releases = releases.lock().unwrap();
    assert_eq!(releases.len(), 10);
    for &(under_gate, enters, exits) in releases.iter() {
        assert!(
            !under_gate,
            "last query owner under an ancestor/destination gate"
        );
        assert_eq!(enters, exits, "last query owner under admission");
    }
}
