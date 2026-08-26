//! Behavioral tests for generation-bearing PMU slot ownership.

#[path = "../src/perf/sampling_registry.rs"]
mod sampling_registry;

use sampling_registry::{RegisterError, SamplingRegistry, UnregisterError};

#[test]
fn stale_unregister_cannot_remove_a_reused_counter_slot() {
    let mut registry = SamplingRegistry::new();
    registry.register(3, 10, "old").unwrap();
    assert_eq!(registry.unregister(3, 10).unwrap(), "old");
    registry.register(3, 11, "new").unwrap();

    assert_eq!(registry.unregister(3, 10), Err(UnregisterError::Stale));
    assert_eq!(registry.get_mut(3).map(|value| *value), Some("new"));
}

#[test]
fn live_slot_cannot_be_silently_replaced() {
    let mut registry = SamplingRegistry::new();
    registry.register(5, 20, 7_u32).unwrap();
    assert_eq!(
        registry.register(5, 21, 8_u32),
        Err(RegisterError::Occupied)
    );
    assert_eq!(registry.unregister(5, 20).unwrap(), 7);
}
