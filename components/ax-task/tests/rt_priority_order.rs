#[path = "../src/scheduler/rt_priority.rs"]
mod rt_priority;

use rt_priority::{
    RT_PRIORITY_LEVELS, bitmap_highest_rt_priority, rt_priority_from_index, rt_priority_index,
};

#[test]
fn rt_bitmap_selection_follows_linux_internal_priority_order() {
    assert_eq!(rt_priority_index(99), 0);
    assert_eq!(rt_priority_index(1), RT_PRIORITY_LEVELS - 1);
    assert_eq!(rt_priority_from_index(0), 99);

    let bitmap = (1_u128 << rt_priority_index(1)) | (1_u128 << rt_priority_index(99));
    assert_eq!(
        bitmap_highest_rt_priority(bitmap),
        Some(99),
        "Linux's lowest internal bit is the highest POSIX RT priority",
    );
}
