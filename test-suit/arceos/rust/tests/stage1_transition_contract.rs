//! The SMP stage-1 test must exercise the permission fault, not only PTE flags.

const STAGE1_TRANSITION: &str = include_str!("../src/mem/stage1_transition.rs");

#[test]
fn remote_store_runs_while_the_mapping_is_still_read_only() {
    let protect = STAGE1_TRANSITION
        .find("protect_kernel_range(mapping, PAGE_SIZE, MappingFlags::READ)")
        .expect("the test must revoke write permission");
    let publish_remote_store = STAGE1_TRANSITION[protect..]
        .find("phase.store(2, Ordering::Release)")
        .map(|offset| protect + offset)
        .expect("the controller must release the remote store");
    let read_only_window = &STAGE1_TRANSITION[protect..publish_remote_store];

    assert!(
        !read_only_window.contains("protect_kernel_range(mapping, PAGE_SIZE, flags)"),
        "restoring WRITE before the remote store makes the shootdown test vacuous"
    );
    assert!(STAGE1_TRANSITION.contains("set_page_fault_handler"));
    assert!(STAGE1_TRANSITION.contains("REMOTE_WRITE_FAULT_HANDLED"));
}
