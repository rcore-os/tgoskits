use super::*;

#[test]
fn held_lock_display_includes_class_addr_and_location() {
    let held = HeldLock {
        class_id: 3,
        kind: HeldLockKind::Spin,
        mode: HeldLockMode::Exclusive,
        sleep_forbidden: true,
        addr: 0x1234,
        caller: Location::caller(),
    };
    let rendered = held.to_string();
    assert!(rendered.contains("kind=spin"));
    assert!(rendered.contains("mode=exclusive"));
    assert!(rendered.contains("sleep_forbidden=true"));
    assert!(rendered.contains("class=3"));
    assert!(rendered.contains("addr=0x1234"));
    assert!(rendered.contains("acquired_at="));
}

#[cfg(target_pointer_width = "64")]
#[test]
fn subclass_support_does_not_increase_held_lock_state_size() {
    assert_eq!(core::mem::size_of::<HeldLock>(), 24);
    assert_eq!(core::mem::size_of::<HeldLockStack>(), 776);
    assert_eq!(core::mem::size_of::<HeldLockSnapshot>(), 776);
    assert_eq!(core::mem::size_of::<PreparedAcquire>(), 800);
}

#[test]
fn held_stack_display_marks_top_entry() {
    let caller = Location::caller();
    let mut snapshot = HeldLockSnapshot::new();
    snapshot.push(HeldLock {
        class_id: 2,
        kind: HeldLockKind::Spin,
        mode: HeldLockMode::Exclusive,
        sleep_forbidden: true,
        addr: 0x10,
        caller,
    });
    snapshot.push(HeldLock {
        class_id: 3,
        kind: HeldLockKind::Mutex,
        mode: HeldLockMode::Exclusive,
        sleep_forbidden: false,
        addr: 0x20,
        caller,
    });

    let rendered = HeldLockStackDisplay {
        snapshot: &snapshot,
        subclasses: &HeldLockSubclassSnapshot {
            values: [DEFAULT_LOCK_SUBCLASS; MAX_HELD_LOCK_SNAPSHOT],
        },
    }
    .to_string();
    assert!(rendered.contains("[0] held: kind=spin mode=exclusive sleep_forbidden=true class=2"));
    assert!(rendered.contains("[1] top: kind=mutex mode=exclusive sleep_forbidden=false class=3"));
}

#[test]
fn dynamic_lock_instances_do_not_consume_class_slots() {
    let locks: Vec<_> = (0..(MAX_LOCK_CLASSES + 128))
        .map(|_| LockdepMap::new_dynamic())
        .collect();

    for lock in &locks {
        let prepared = prepare_acquire_with_snapshot_checked(
            lock,
            "test lock",
            lock as *const _ as usize,
            Location::caller(),
            HeldLockSnapshot::new(),
        )
        .unwrap();
        assert_ne!(prepared.class_id(), 0);
    }
}

#[test]
fn subclass_tracks_same_base_class_nesting() {
    fn prepare_with_subclass(
        map: &LockdepMap,
        held_before: HeldLockSnapshot,
        subclass: LockSubclass,
    ) -> PreparedAcquire {
        prepare_acquire_with_snapshot_checked_nested(
            map,
            "test lock",
            map as *const _ as usize,
            Location::caller(),
            held_before,
            subclass,
        )
        .unwrap()
    }

    let parent = LockdepMap::new_dynamic();
    let child = LockdepMap::new_dynamic();
    let parent_acquire =
        prepare_with_subclass(&parent, HeldLockSnapshot::new(), DEFAULT_LOCK_SUBCLASS);
    let parent_class = parent_acquire.class_id();
    let mut parent_held = HeldLockSnapshot::new();
    parent_held.push(HeldLock {
        class_id: parent_class,
        kind: HeldLockKind::Spin,
        mode: HeldLockMode::Exclusive,
        sleep_forbidden: true,
        addr: &parent as *const _ as usize,
        caller: Location::caller(),
    });
    let child_acquire = prepare_with_subclass(&child, parent_held, 1);
    assert_eq!(class_subclass(parent_class), DEFAULT_LOCK_SUBCLASS);
    assert_eq!(class_subclass(child_acquire.class_id()), 1);

    let mut held_locks = HeldLockStack::new();
    finish_acquire_with_stack(
        parent_acquire,
        &parent as *const _ as usize,
        &mut held_locks,
    );
    finish_acquire_with_stack(child_acquire, &child as *const _ as usize, &mut held_locks);
    release_from_stack(&child as *const _ as usize, &mut held_locks);
    release_from_stack(&parent as *const _ as usize, &mut held_locks);

    let mut nested_held = HeldLockSnapshot::new();
    nested_held.push(HeldLock {
        class_id: child_acquire.class_id(),
        kind: HeldLockKind::Spin,
        mode: HeldLockMode::Exclusive,
        sleep_forbidden: true,
        addr: &child as *const _ as usize,
        caller: Location::caller(),
    });
    let reverse = prepare_acquire_with_snapshot_checked_nested(
        &parent,
        "test lock",
        &parent as *const _ as usize,
        Location::caller(),
        nested_held,
        DEFAULT_LOCK_SUBCLASS,
    );
    assert!(matches!(reverse, Err(LockdepCheckError::OrderInversion)));
}
