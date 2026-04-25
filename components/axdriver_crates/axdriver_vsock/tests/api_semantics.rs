use ax_driver_vsock::{VsockAddr, VsockConnId, VsockDriverEvent};

#[test]
fn test_listening_conn_id_constructor() {
    let cid = VsockConnId::listening(9000);
    assert_eq!(cid.local_port, 9000);
    assert_eq!(cid.peer_addr.cid, 0);
    assert_eq!(cid.peer_addr.port, 0);
}

#[test]
fn test_vsock_conn_id_ordering() {
    let a = VsockConnId {
        peer_addr: VsockAddr { cid: 3, port: 1000 },
        local_port: 2000,
    };
    let b = VsockConnId {
        peer_addr: VsockAddr { cid: 3, port: 1001 },
        local_port: 2000,
    };

    assert!(a < b);
}

#[test]
fn test_unknown_event_variant_constructible() {
    let evt = VsockDriverEvent::Unknown;
    assert!(matches!(evt, VsockDriverEvent::Unknown));
}

