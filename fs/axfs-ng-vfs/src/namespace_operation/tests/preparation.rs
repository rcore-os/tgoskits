//! Extend the existing recursive completion journey with lock-external plans.

use super::*;

struct Plan {
    filesystem: Weak<ProbeFs>,
    events: Events,
    id: usize,
    behavior: usize,
    _owner: Arc<Owner>,
}

fn admissions_ended(events: &[Event]) -> bool {
    events
        .iter()
        .filter(|event| matches!(event, Event::Enter(_)))
        .count()
        == events
            .iter()
            .filter(|event| matches!(event, Event::Exit(_)))
            .count()
}

impl crate::NamespacePreparation for Plan {
    fn prepare(&self, _: &PreparedWrite) -> VfsResult<()> {
        let filesystem = self.filesystem.upgrade().unwrap();
        assert!(filesystem.gate.try_lock().is_ok());
        let mut events = self.events.lock().unwrap();
        assert!(
            admissions_ended(&events),
            "prepare retained an ancestor admission"
        );
        let round = events
            .iter()
            .rposition(|event| matches!(event, Event::Enter(_)))
            .unwrap();
        assert_eq!(
            events[round..]
                .iter()
                .filter(|event| matches!(event, Event::Finish(_)))
                .count(),
            2
        );
        events.push(Event::Prepare(self.id));
        if self.behavior == 4 {
            Err(VfsError::NoMemory)
        } else {
            Ok(())
        }
    }

    fn cleanup(&self, _: &PreparedWrite) -> VfsResult<()> {
        let filesystem = self.filesystem.upgrade().unwrap();
        assert!(filesystem.gate.try_lock().is_ok());
        let mut events = self.events.lock().unwrap();
        assert!(
            admissions_ended(&events),
            "cleanup retained an ancestor admission"
        );
        events.push(Event::Cleanup(self.id));
        if self.behavior == 5 {
            Err(VfsError::Io)
        } else {
            Ok(())
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Drop for Plan {
    fn drop(&mut self) {
        let mut events = self.events.lock().unwrap();
        let ended = admissions_ended(&events);
        events.push(Event::DropPreparation(self.id, ended));
    }
}

pub(super) fn change_metadata(
    node: &ProbeNode,
    request: &MetadataRequest<'_>,
) -> NamespaceCompletion<Metadata> {
    request.admission().check_owner(node.0.as_ref()).unwrap();
    let _guard = node.0.gate.lock().unwrap();
    if let Some(backing) = node.0.roots.first() {
        let child = request
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
        assert!(matches!(
            child.state(),
            NamespaceState::Retry | NamespaceState::Applied
        ));
    }
    // The same local key in both receivers must denote two distinct plans.
    let candidate = Arc::new(Plan {
        filesystem: Arc::downgrade(&node.0),
        events: node.0.events.clone(),
        id: node.0.id,
        behavior: node.0.action.load(Ordering::Relaxed),
        _owner: node.0.owner.clone(),
    });
    let state = request
        .preparations()
        .insert(crate::NodeIdentity::new([7].as_slice()), candidate)
        .unwrap();
    let outcome = match state {
        crate::PreparationState::Pending(plan) => {
            assert_eq!(plan.id, node.0.id);
            NamespaceOutcome::retry()
        }
        crate::PreparationState::Ready(plan) => {
            assert_eq!(plan.id, node.0.id);
            node.0.root.lock().unwrap().clone()
        }
    };
    let name = if node.0.roots.is_empty() {
        "metadata"
    } else {
        "root"
    };
    NamespaceCompletion::new(outcome, Some(Box::new(node.0.work(name))))
}

pub(super) fn exercise() {
    for (behavior, original, expected) in [
        (3, Ok(()), Ok(())),
        (4, Ok(()), Err(VfsError::NoMemory)),
        (5, Ok(()), Err(VfsError::Io)),
        (
            5,
            Err(VfsError::PermissionDenied),
            Err(VfsError::PermissionDenied),
        ),
    ] {
        let events = Events::default();
        let owner = Arc::new(Owner(events.clone()));
        let weak_owner = Arc::downgrade(&owner);
        let (native, backing) = filesystem(1, vec![], &events, &owner, true, false);
        let (logical, root) = filesystem(0, vec![backing.clone()], &events, &owner, true, false);
        native.action.store(3, Ordering::Relaxed);
        logical.action.store(behavior, Ordering::Relaxed);
        *native.root.lock().unwrap() = NamespaceOutcome::applied(snapshot(), Ok(()));
        *logical.root.lock().unwrap() = NamespaceOutcome::applied(snapshot(), original);
        let write = PreparedWrite::acquire(&root).unwrap();
        let result = crate::NamespaceCommandScope::run_write(&write, |command| {
            for round in 0..2 {
                let scope = command.attempt::<Metadata>()?;
                {
                    let context = scope.context();
                    scope.change_metadata(
                        &context,
                        &root,
                        MetadataChange::Chown {
                            uid: None,
                            gid: None,
                        },
                        &MutationCredentials::root(),
                    )?;
                }
                assert_eq!(command.prepare(), Err(VfsError::BadState));
                let outcome = scope
                    .finish()
                    .unwrap_or_else(|_| panic!("actual owner mismatch"));
                if round == 0 {
                    assert_eq!(outcome.state(), NamespaceState::Retry);
                    command.prepare()?;
                } else {
                    assert_eq!(outcome.state(), NamespaceState::Applied);
                    return Ok(outcome);
                }
            }
            panic!("preparation must complete after a fresh attempt")
        });
        let actual = match result {
            Ok(outcome) => {
                assert_eq!(outcome.state(), NamespaceState::Applied);
                assert_eq!(outcome.observation().unwrap().inode, 1);
                outcome.into_result()
            }
            Err(error) => Err(error),
        };
        assert_eq!(actual, expected);
        let trace = events.lock().unwrap();
        assert_eq!(
            trace
                .iter()
                .filter(|event| matches!(event, Event::Prepare(_)))
                .count(),
            2
        );
        assert_eq!(
            trace
                .iter()
                .filter(|event| matches!(event, Event::Cleanup(_)))
                .count(),
            2
        );
        assert!(
            trace
                .iter()
                .all(|event| !matches!(event, Event::DropPreparation(_, false)))
        );
        let calls = if behavior == 4 { 1 } else { 2 };
        assert_eq!(native.backend_calls.load(Ordering::Relaxed), calls);
        assert_eq!(logical.backend_calls.load(Ordering::Relaxed), calls);
        drop(trace);
        drop(write);
        drop(root);
        drop(logical);
        drop(backing);
        drop(native);
        drop(owner);
        assert!(weak_owner.upgrade().is_none());
    }
}
