//! Allocation failure must retain uncached non-root command inputs until exit.

use super::*;

#[derive(Debug, PartialEq, Eq)]
struct InputRelease {
    name: &'static str,
    gate_held: bool,
    active: Option<usize>,
    pressures: usize,
    finishes: usize,
}

struct LastInput {
    name: &'static str,
    outer: Weak<ProbeFs>,
    events: Events,
    released: Arc<Mutex<Vec<InputRelease>>>,
}

impl Drop for LastInput {
    fn drop(&mut self) {
        // Only record destruction context. Assertions belong outside Drop so a
        // regressed lifetime cannot turn an ordinary failure into double panic.
        let gate_held = self
            .outer
            .upgrade()
            .is_some_and(|outer| outer.gate.try_lock().is_err());
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
        let release = InputRelease {
            name: self.name,
            gate_held,
            active: enters.checked_sub(exits),
            pressures: events
                .iter()
                .filter(|event| matches!(event, Event::Pressure(_)))
                .count(),
            finishes: events
                .iter()
                .filter(|event| matches!(event, Event::Finish(_)))
                .count(),
        };
        self.released
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(release);
    }
}

pub(super) fn exercise() {
    let events = Events::default();
    let owner = Arc::new(Owner(events.clone()));
    let (native, backing) = filesystem(1, vec![], &events, &owner, true, false);
    let (logical, root) = filesystem(0, vec![backing.clone()], &events, &owner, true, false);
    let write = PreparedWrite::acquire(&root).unwrap();
    let scope = NamespaceOperationScope::acquire(&write).unwrap();
    let released = Arc::new(Mutex::new(Vec::new()));
    let names = [
        "create-parent",
        "metadata-target",
        "link-parent",
        "link-source",
        "rename-source",
        "rename-destination",
        "unlink-parent",
    ];
    let inputs = names.map(|name| {
        // Construct an uncached alias directly, never via an admitted lookup.
        // Preparation owns its mount/root, but cannot retain this new dentry.
        let entry = native.entry(Reference::new(Some(backing.entry().clone()), name.into()));
        entry.user_data().insert(LastInput {
            name,
            outer: Arc::downgrade(&logical),
            events: events.clone(),
            released: released.clone(),
        });
        Location::new(backing.mountpoint().clone(), entry)
    });
    let weak_inputs = inputs.each_ref().map(|input| input.entry().downgrade());
    let prepared_unlink = inputs[6].prepare_unlink("victim").unwrap();
    let empty = Location::new(
        backing.mountpoint().clone(),
        native.entry(Reference::new(
            Some(backing.entry().clone()),
            "empty".into(),
        )),
    );
    let weak_empty = empty.entry().downgrade();
    {
        let context = scope.context();
        let credentials = MutationCredentials::root();
        // Keep both typed child sets and a real root receipt pending before
        // injection. Allocation failure must not discard or overwrite any.
        let metadata = context
            .change_metadata(
                &backing,
                MetadataChange::Chown {
                    uid: None,
                    gid: None,
                },
                &credentials,
            )
            .unwrap()
            .unwrap();
        assert_eq!(metadata.result(), Err(VfsError::AlreadyExists));
        let intent = crate::CreateIntent::Open(crate::ExistingOpen {
            access: NodePermission::OTHER_READ,
        });
        assert_eq!(
            context
                .create(
                    &backing,
                    "victim",
                    creation_attributes(),
                    intent,
                    &credentials
                )
                .unwrap()
                .result(),
            Ok(())
        );
        let root_observation = scope
            .create(
                &context,
                &root,
                "victim",
                creation_attributes(),
                intent,
                &credentials,
            )
            .unwrap();
        assert_eq!(root_observation.result(), Ok(()));
        let calls = (
            native.backend_calls.load(Ordering::Relaxed),
            logical.backend_calls.load(Ordering::Relaxed),
        );
        let before = events.lock().unwrap().clone();
        let (saved_reservations, child_count) = {
            let mut registration = context.children.borrow_mut();
            let saved = registration.reservations;
            assert_eq!(saved, 0);
            assert_eq!(registration.children.len(), 2);
            assert!(registration.root_claimed);
            assert!(!registration.incomplete);
            // This is only fault injection, not a claim of outstanding rights.
            // checked_add succeeds; real Vec::try_reserve then returns capacity
            // overflow without asking the allocator for enormous storage.
            registration.reservations = usize::MAX - 1;
            (saved, registration.children.len())
        };
        let outer_gate = logical.gate.lock().unwrap();
        assert!(matches!(
            context.create(
                &inputs[0],
                "new",
                creation_attributes(),
                crate::CreateIntent::Exclusive,
                &credentials
            ),
            Err(VfsError::NoMemory)
        ));
        assert!(matches!(
            context.change_metadata(
                &inputs[1],
                MetadataChange::Chmod(NodePermission::default()),
                &credentials
            ),
            Err(VfsError::NoMemory)
        ));
        assert!(matches!(
            context.link(&inputs[2], "alias", &inputs[3], &credentials),
            Err(VfsError::NoMemory)
        ));
        assert!(matches!(
            context.rename(
                (&inputs[4], "source"),
                (&inputs[5], "destination"),
                RenameOptions::REPLACE,
                &credentials
            ),
            Err(VfsError::NoMemory)
        ));
        assert!(matches!(
            context.unlink(prepared_unlink, false, &credentials),
            Err(VfsError::NoMemory)
        ));
        assert!(
            context
                .change_metadata(
                    &empty,
                    MetadataChange::Times {
                        atime: crate::TimeChange::Keep,
                        mtime: crate::TimeChange::Keep
                    },
                    &credentials,
                )
                .unwrap()
                .is_none()
        );
        drop(empty);
        assert!(
            weak_empty.upgrade().is_none(),
            "empty times must not retire or reserve"
        );
        {
            let mut registration = context.children.borrow_mut();
            assert_eq!(registration.reservations, usize::MAX - 1);
            registration.reservations = saved_reservations;
            assert_eq!(registration.children.len(), child_count);
            assert!(registration.root_claimed);
            assert!(!registration.incomplete);
        }
        assert!(scope.state().root.borrow().is_some());
        assert_eq!(
            calls,
            (
                native.backend_calls.load(Ordering::Relaxed),
                logical.backend_calls.load(Ordering::Relaxed),
            ),
            "failed reservations must not call either backend"
        );
        assert_eq!(*events.lock().unwrap(), before);
        drop(inputs);
        assert!(
            released.lock().unwrap().is_empty(),
            "caller drop must not destroy any failed input under the outer gate"
        );
        for (name, weak) in names.iter().zip(&weak_inputs) {
            assert!(weak.upgrade().is_some(), "{name} escaped common retirement");
        }
        drop(outer_gate);
    }
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("fixed retained owners"));
    assert_eq!(outcome.state(), NamespaceState::Unchanged);
    assert_eq!(outcome.result(), Ok(()));
    assert!(
        matches!(outcome.observation(), Some(NamespaceEffect::Create(effect)) if !effect.created)
    );
    assert!(weak_inputs.iter().all(|weak| weak.upgrade().is_none()));
    let mut actual = core::mem::take(&mut *released.lock().unwrap());
    actual.sort_unstable_by_key(|release| release.name);
    let mut expected: Vec<_> = names
        .into_iter()
        .map(|name| InputRelease {
            name,
            gate_held: false,
            active: Some(0),
            pressures: 3,
            finishes: 0,
        })
        .collect();
    expected.sort_unstable_by_key(|release| release.name);
    assert_eq!(
        actual, expected,
        "every last input owner must retire after all admissions, before finish"
    );
    let trace = events.lock().unwrap();
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
    assert_eq!(pressures, vec!["metadata", "create", "create"]);
    assert_eq!(finishes, pressures);
    assert_eq!(
        trace
            .iter()
            .filter(|event| matches!(event, Event::Exit(_)))
            .count(),
        2
    );
}
