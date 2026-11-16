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
        let events = self.events.lock().unwrap().clone();
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
        self.events.lock().unwrap().push(Event::Prepare(self.id));
        if self.behavior == 4 {
            Err(VfsError::NoMemory)
        } else {
            Ok(())
        }
    }

    fn cleanup(&self, _: &PreparedWrite) -> VfsResult<()> {
        let filesystem = self.filesystem.upgrade().unwrap();
        assert!(filesystem.gate.try_lock().is_ok());
        let events = self.events.lock().unwrap().clone();
        assert!(
            admissions_ended(&events),
            "cleanup retained an ancestor admission"
        );
        self.events.lock().unwrap().push(Event::Cleanup(self.id));
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
        if let Ok(mut events) = self.events.try_lock() {
            let ended = admissions_ended(&events);
            events.push(Event::DropPreparation(self.id, ended));
        }
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
            if node.0.action.load(Ordering::Relaxed) == 6 {
                request
                    .preparations()
                    .defer(&crate::NodeIdentity::new([7].as_slice()))
                    .unwrap();
                node.0.action.store(3, Ordering::Relaxed);
                NamespaceOutcome::retry()
            } else {
                node.0.root.lock().unwrap().clone()
            }
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
    backing_unlink_origin();
    for (behavior, original, expected) in [
        (3, Ok(()), Ok(())),
        (6, Ok(()), Ok(())),
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
            let rounds = if behavior == 6 { 3 } else { 2 };
            for round in 0..rounds {
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
                if round + 1 < rounds {
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
        let trace = events.lock().unwrap().clone();
        assert_eq!(
            trace
                .iter()
                .filter(|event| matches!(event, Event::Prepare(_)))
                .count(),
            if behavior == 6 { 3 } else { 2 }
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
        assert_eq!(
            trace
                .iter()
                .filter(|event| matches!(event, Event::DropPreparation(..)))
                .count(),
            if behavior == 4 {
                2
            } else if behavior == 6 {
                6
            } else {
                4
            }
        );
        let calls = if behavior == 4 {
            1
        } else if behavior == 6 {
            3
        } else {
            2
        };
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

fn backing_unlink_origin() {
    let events = Events::default();
    let owner = Arc::new(Owner(events.clone()));
    let (native, backing) = filesystem(1, vec![], &events, &owner, false, false);
    let (logical, root) = filesystem(0, vec![backing.clone()], &events, &owner, false, false);
    let write = PreparedWrite::acquire(&root).unwrap();
    let command = crate::NamespaceCommandScope::new(&write);
    let retired = MutationRetire::new();
    // This factory grants reference preparation only. Both attempts below
    // still execute the actual native observe/authorize/resume protocol.
    let preparation = crate::NamespacePreparationContext::new(&command, logical.as_ref(), &retired);
    assert!(matches!(
        preparation.unlink_origin(&backing),
        Err(VfsError::BadState)
    ));
    let origin = preparation.unlink_origin(&root).unwrap();
    assert!(matches!(
        origin.prepare(&write, &root, "victim"),
        Err(VfsError::BadState)
    ));
    let prepared = origin.prepare(&write, &backing, "victim").unwrap();
    let foreign = crate::NamespaceCommandScope::new(&write);
    for (command, wrong) in [(&foreign, true), (&command, false)] {
        let scope = command.attempt::<()>().unwrap();
        {
            let context = scope.context();
            let calls = native.backend_calls.load(Ordering::Relaxed);
            let started = context.unlink_backing(&prepared, true, &MutationCredentials::root());
            if wrong {
                assert!(matches!(started, Err(VfsError::BadState)));
                assert_eq!(native.backend_calls.load(Ordering::Relaxed), calls);
            } else {
                let result = started.unwrap().resume();
                assert_eq!(
                    (result.state(), result.result()),
                    (NamespaceState::Applied, Ok(()))
                );
                assert!(matches!(
                    result.observation(),
                    Some(NamespaceEffect::Unlink(_))
                ));
            }
            // The modeled logical prefix is intentionally effect-free. This
            // component journey proves child admission/completion and binding,
            // not Overlay permissions or userspace namespace semantics.
            *scope.state().root.borrow_mut() = Some(
                NamespaceCompletion::new(NamespaceOutcome::unchanged(Ok(())), None)
                    .bind(logical.as_ref()),
            );
        }
        scope
            .finish()
            .unwrap_or_else(|_| panic!("bound unlink completion"))
            .into_result()
            .unwrap();
        assert!(admissions_ended(&events.lock().unwrap()));
    }
    foreign
        .finish::<()>(Ok(NamespaceOutcome::unchanged(Ok(()))))
        .unwrap()
        .into_result()
        .unwrap();
    command
        .finish::<()>(Ok(NamespaceOutcome::unchanged(Ok(()))))
        .unwrap()
        .into_result()
        .unwrap();
}
