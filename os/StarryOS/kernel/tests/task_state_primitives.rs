//! Behavioral coverage for task state that must remain independent of kernel linkage.

extern crate alloc;

#[path = "../src/task/bounded_stack.rs"]
mod bounded_stack;
#[path = "../src/task/user_memory_access.rs"]
mod user_memory_access;

use bounded_stack::BoundedStack;
use user_memory_access::UserMemoryAccessDepth;

#[test]
fn bounded_stack_preserves_lifo_without_exceeding_capacity() {
    let mut stack = BoundedStack::<u32, 2>::new();

    assert_eq!(stack.try_push(10), Ok(()));
    assert_eq!(stack.try_push(20), Ok(()));
    assert_eq!(stack.try_push(30), Err(30));
    assert_eq!(stack.pop(), Some(20));
    assert_eq!(stack.pop(), Some(10));
    assert_eq!(stack.pop(), None);
}

#[test]
fn nested_user_memory_access_is_removed_by_its_unique_guards() {
    let depth = UserMemoryAccessDepth::new();
    assert!(!depth.is_active());

    let outer = depth.enter();
    assert!(depth.is_active());
    {
        let inner = depth.enter();
        assert!(depth.is_active());
        drop(inner);
    }
    assert!(depth.is_active());

    drop(outer);
    assert!(!depth.is_active());
}
