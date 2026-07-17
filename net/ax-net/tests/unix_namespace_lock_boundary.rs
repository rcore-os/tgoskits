//! Source contracts for filesystem-backed Unix socket namespace calls.

const UNIX: &str = include_str!("../src/unix/mod.rs");

fn item_body<'source>(source: &'source str, signature: &str, next: &str) -> &'source str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing item: {signature}"))
        .1
        .split_once(next)
        .unwrap_or_else(|| panic!("missing item after {signature}: {next}"))
        .0
}

#[test]
fn bind_and_connect_release_address_state_before_namespace_callbacks() {
    let socket_ops = item_body(
        UNIX,
        "impl SocketOps for UnixSocket",
        "impl Pollable for UnixSocket",
    );
    let bind = item_body(socket_ops, "fn bind(", "fn connect(");
    let connect = item_body(socket_ops, "fn connect(", "fn listen(");

    assert!(
        UNIX.contains("struct AddressTransaction"),
        "concurrent address publication needs an explicit prepare/commit/rollback state"
    );
    assert!(
        bind.contains("publish_address(&self.local_addr")
            || bind.contains("publish_binding(&self.local_addr"),
        "bind must use a transaction before resolving a filesystem path"
    );
    assert!(
        connect.contains("publish_address(&self.remote_addr")
            && connect.contains("resolve_slot(&remote_addr)"),
        "connect must publish its Busy state before resolving a filesystem path"
    );
    assert!(
        !bind.contains("let mut guard = self.local_addr.lock()")
            && !connect.contains("let mut guard = self.remote_addr.lock()"),
        "a socket address spin guard must never span a UnixNamespace callback"
    );
}

#[test]
fn abstract_namespace_returns_owned_slots_before_transport_callbacks() {
    assert!(
        UNIX.contains("type AbstractBindMap = HashMap<Arc<[u8]>, Arc<BindSlot>>"),
        "abstract namespace slots must remain alive after releasing the global map lock"
    );
    assert!(
        UNIX.contains("fn resolve_slot(") && UNIX.contains("struct BindReservation"),
        "namespace lookup and bind reservation must own slots outside the map lock"
    );
    assert!(
        !UNIX.contains("fn with_slot<") && !UNIX.contains("fn with_slot_or_insert<"),
        "the global namespace map must not own arbitrary transport callback execution"
    );
}

#[test]
fn bind_reservation_rolls_back_before_address_state_and_supports_retry() {
    assert!(
        UNIX.contains("fn rollback_inner(&self) -> AxResult")
            && UNIX.contains("fn rollback(mut self) -> AxResult")
            && UNIX.contains("impl Drop for BindReservation"),
        "an unpublished namespace entry needs typed explicit and drop rollback"
    );
    let publish = item_body(UNIX, "fn publish_binding(", "#[derive(Default)]");
    let namespace_rollback = publish
        .find("reservation.rollback()")
        .expect("transport rejection must roll back its namespace reservation");
    let address_return = publish
        .find("return Err(bind_error)")
        .expect("transport error must be returned after rollback");
    assert!(namespace_rollback < address_return);
}
