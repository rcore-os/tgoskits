//! Deterministic control semantics for inherited task PMU events.

#[path = "../src/perf/inheritance_lifecycle.rs"]
mod inheritance_lifecycle;

use inheritance_lifecycle::PerfInheritanceLifecycle;

#[test]
fn child_inherited_before_mmap_observes_the_later_output_generation() {
    let mut family = PerfInheritanceLifecycle::new(false);
    let child = family.register_member(32).expect("open family");
    assert_eq!(child.output_generation, None);

    let generation = family.publish_output().expect("open family");
    assert_eq!(generation, 1);

    let grandchild = family.register_member(32).expect("open family");
    assert_eq!(grandchild.output_generation, Some(generation));
}

#[test]
fn enable_disable_intent_covers_existing_and_future_descendants() {
    let mut family = PerfInheritanceLifecycle::new(false);
    family.register_member(32).expect("first child");

    assert_eq!(family.set_enabled(true), Some(2));
    assert!(
        family
            .register_member(32)
            .expect("child after enable")
            .enabled
    );

    assert_eq!(family.set_enabled(false), Some(3));
    assert!(
        !family
            .register_member(32)
            .expect("child after disable")
            .enabled
    );
}

#[test]
fn close_rejects_new_members_and_control_updates() {
    let mut family = PerfInheritanceLifecycle::new(true);
    family.register_member(32).expect("child before close");

    assert_eq!(family.close(), 2);
    assert!(family.is_closed());
    assert!(!family.enabled());
    assert!(family.register_member(32).is_none());
    assert!(family.publish_output().is_none());
    assert!(family.set_enabled(true).is_none());
}

#[test]
fn retired_children_do_not_exhaust_the_live_family_capacity() {
    let mut family = PerfInheritanceLifecycle::new(false);
    for _ in 0..64 {
        family
            .register_member(2)
            .expect("one live child must fit beside the root");
        assert!(
            family.retire_member(),
            "retiring a live child must release one relationship slot"
        );
    }
}
