//! Public compile-time contract for the scheduler synchronization layers.

#[test]
fn stable_api_and_runtime_bridge_are_separate_namespaces() {
    let lock = ax_task::sync::api::SpinLock::new(0usize);
    let _: &ax_task::sync::SpinLock<usize> = &lock;

    let _current_thread_id = ax_task::sync::bridge::current_thread_id;
    let _current_thread_token = ax_task::sync::bridge::current_thread_token;
    let _pi_mutex_lock_slow = ax_task::sync::bridge::pi_mutex_lock_slow;
    let _pi_mutex_release_owned = ax_task::sync::bridge::pi_mutex_release_owned;
}
