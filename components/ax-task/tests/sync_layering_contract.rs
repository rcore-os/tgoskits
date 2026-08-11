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

#[test]
fn execution_context_guards_are_native_task_types() {
    assert!(
        core::any::type_name::<ax_task::sync::PreemptGuard>()
            .starts_with("ax_task::sync::context::"),
        "execution-context ownership must not remain in the external bridge crate"
    );
}

#[test]
fn spin_algorithms_are_native_task_types() {
    assert!(
        core::any::type_name::<ax_task::sync::SpinLock<()>>().starts_with("ax_task::sync::spin::"),
        "spin-lock ownership must not remain in the external bridge crate"
    );
    assert!(
        core::any::type_name::<ax_task::sync::SpinRwLock<()>>()
            .starts_with("ax_task::sync::spin::"),
        "spin-rwlock ownership must not remain in the external bridge crate"
    );
}
