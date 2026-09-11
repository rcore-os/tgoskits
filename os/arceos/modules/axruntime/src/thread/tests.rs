use super::*;

#[test]
fn secondary_bootstrap_retires_before_entering_idle_loop() {
    let bootstrap = ThreadId::from_parts(1, 1);
    let idle = ThreadId::from_parts(2, 1);

    assert_eq!(
        idle_entry_action(Some(bootstrap), Some(idle)).unwrap(),
        IdleEntryAction::RetireBootstrap,
    );
    assert_eq!(
        idle_entry_action(Some(idle), Some(idle)).unwrap(),
        IdleEntryAction::RunIdle,
    );
}

#[cfg(kernel_tls)]
#[test]
fn bootstrap_thread_rejects_a_missing_tls_resource() {
    // SAFETY: this inert non-zero identity is never dereferenced because
    // validation rejects the missing TLS resource first.
    let context = unsafe { ExecutionContextHandle::from_raw(1) };
    let result = assemble_bootstrap_resources(context, TlsHandle::NONE);

    assert!(matches!(result, Err(TaskError::InvalidRuntimeHandle)));
}
