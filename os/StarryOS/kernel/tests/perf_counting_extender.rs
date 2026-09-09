//! Deterministic finite-width PMU count extension state.

#[path = "../src/perf/counting.rs"]
mod counting;

use counting::CounterExtender;

#[test]
fn programmable_counter_wrap_is_preserved_until_reset() {
    let mut state = CounterExtender::new();
    assert_eq!(state.value(0xffff_fff0, 32), 0xffff_fff0);

    state.record_overflow();
    assert_eq!(state.value(0x10, 32), 0x1_0000_0010);

    state.reset();
    assert_eq!(state.value(0x10, 32), 0x10);
}
