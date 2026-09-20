//! Pathname socket operations must permit a namespace provider to sleep.

use std::{sync::Arc, thread, time::Duration};

use ax_std::os::arceos::{
    modules::ax_net::{
        ConnectStatus, NetError, NetResult, RecvOptions, SendOptions, SocketAddrEx, SocketOps,
        unix::{
            BindSlot, DgramTransport, StreamTransport, UnixNamespace, UnixSocket, UnixSocketAddr,
            register_unix_namespace,
        },
    },
    task::thread::current::validate_blocking_context,
};

struct SleepableNamespace(Arc<BindSlot>);

impl SleepableNamespace {
    fn access(&self) -> NetResult<Arc<BindSlot>> {
        // Filesystem lookup/creation can block even for a nonblocking socket.
        // Check the real runtime context before the wait: this deterministically
        // rejects a facade spin guard, without relying on disk contention.
        validate_blocking_context().expect("Unix pathname access must remain sleepable");
        thread::sleep(Duration::from_millis(1));
        Ok(self.0.clone())
    }
}

impl UnixNamespace for SleepableNamespace {
    fn resolve(&self, _path: &str) -> NetResult<Arc<BindSlot>> {
        self.access()
    }

    fn bind(&self, _path: &str) -> NetResult<Arc<BindSlot>> {
        self.access()
    }

    fn unbind(&self, _path: &str) -> NetResult<()> {
        panic!("pathname socket nodes must survive close until filesystem unlink")
    }
}

pub fn run() -> crate::TestResult {
    let unconnected = UnixSocket::new(DgramTransport::new(1));
    assert_eq!(unconnected.peer_addr().unwrap_err(), NetError::NotConnected);
    let (left, right) = DgramTransport::new_pair(1);
    for peer in [
        UnixSocket::new_connected(left),
        UnixSocket::new_connected(right),
    ] {
        assert!(matches!(
            peer.peer_addr().unwrap(),
            SocketAddrEx::Unix(UnixSocketAddr::Unnamed)
        ));
    }
    let abstract_address = UnixSocketAddr::Abstract(Arc::from(&b"rebind-after-close"[..]));
    {
        let owner = UnixSocket::new(DgramTransport::new(1));
        owner
            .bind(SocketAddrEx::Unix(abstract_address.clone()))
            .unwrap();
    }
    UnixSocket::new(DgramTransport::new(2))
        .bind(SocketAddrEx::Unix(abstract_address))
        .unwrap();

    register_unix_namespace(SleepableNamespace(Arc::new(BindSlot::default())));
    let address = UnixSocketAddr::Path(Arc::from("/socket"));
    let server = UnixSocket::new(StreamTransport::new(1));
    server.bind(SocketAddrEx::Unix(address.clone())).unwrap();
    server.listen(1).unwrap();
    let client = UnixSocket::new(StreamTransport::new(2));
    assert_eq!(
        client.start_connect(SocketAddrEx::Unix(address)).unwrap(),
        ConnectStatus::Connected
    );
    let accepted = server.try_accept().unwrap();
    assert!(
        matches!(client.peer_addr().unwrap(), SocketAddrEx::Unix(UnixSocketAddr::Path(path)) if path.as_ref() == "/socket")
    );
    assert!(
        matches!(accepted.local_addr().unwrap(), SocketAddrEx::Unix(UnixSocketAddr::Path(path)) if path.as_ref() == "/socket")
    );
    assert!(matches!(
        accepted.peer_addr().unwrap(),
        SocketAddrEx::Unix(UnixSocketAddr::Unnamed)
    ));
    accepted
        .try_send(&b"x"[..], &mut SendOptions::default())
        .unwrap();
    let mut payload = [0u8; 1];
    let mut from = SocketAddrEx::Unix(UnixSocketAddr::Unnamed);
    let received = client
        .try_recv(
            &mut payload[..],
            &mut RecvOptions {
                from: Some(&mut from),
                ..RecvOptions::default()
            },
        )
        .unwrap();
    assert_eq!(received, 1);
    assert_eq!(&payload, b"x");
    assert!(
        matches!(from, SocketAddrEx::Unix(UnixSocketAddr::Path(path)) if path.as_ref() == "/socket")
    );
    println!("Unix pathname bind/connect completed with a sleepable provider");
    Ok(())
}
