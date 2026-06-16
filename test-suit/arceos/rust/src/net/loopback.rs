use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
    println,
};

#[cfg(target_arch = "x86_64")]
const HOST_HTTP: &str = "10.0.2.2:18180";
#[cfg(target_arch = "aarch64")]
const HOST_HTTP: &str = "10.0.2.2:18181";
#[cfg(target_arch = "riscv64")]
const HOST_HTTP: &str = "10.0.2.2:18182";
#[cfg(target_arch = "loongarch64")]
const HOST_HTTP: &str = "10.0.2.2:18183";

const HOST_HTTP_BODY: &str = "ArceOS rust test suite host fixture\n";
const HTTP_REQUEST: &str = "GET / HTTP/1.1\r\nHost: axbuild.local\r\nAccept: */*\r\n\r\n";
const SERVER_SMOKE_PORT: u16 = 5555;

pub fn run() -> crate::TestResult {
    test_address_resolution();
    test_tcp_listener_bind();
    test_fixed_tcp_server_bind();
    test_host_http_client();
    test_udp_bind_and_send();
    test_fixed_udp_server_bind();
    Ok(())
}

fn test_address_resolution() {
    let guest_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 2, 15));
    let mut found_guest_ip = false;
    for addr in "10.0.2.15:5555".to_socket_addrs().unwrap() {
        if addr.ip() == guest_ip {
            found_guest_ip = true;
        }
    }
    assert!(found_guest_ip, "guest IP address did not resolve");
}

fn test_tcp_listener_bind() {
    let listener = TcpListener::bind(("0.0.0.0", 0)).expect("failed to bind TCP smoke listener");
    let addr = listener
        .local_addr()
        .expect("failed to read TCP listener address");
    assert_ne!(addr.port(), 0, "TCP listener did not allocate a port");
    println!("net_loopback: TCP smoke listener {addr}");
}

fn test_fixed_tcp_server_bind() {
    let listener =
        TcpListener::bind(("0.0.0.0", SERVER_SMOKE_PORT)).expect("failed to bind TCP server port");
    let addr = listener
        .local_addr()
        .expect("failed to read fixed TCP listener address");
    assert_eq!(addr.port(), SERVER_SMOKE_PORT);
    println!("net_loopback: fixed TCP server listener {addr}");
}

fn test_host_http_client() {
    let mut resolved = HOST_HTTP
        .to_socket_addrs()
        .expect("failed to resolve host HTTP fixture");
    assert!(
        resolved.any(|addr| addr.ip() == IpAddr::V4(Ipv4Addr::new(10, 0, 2, 2))),
        "host HTTP fixture address did not resolve to QEMU gateway"
    );

    let mut stream = TcpStream::connect(HOST_HTTP).expect("failed to connect host HTTP fixture");
    stream
        .write_all(HTTP_REQUEST.as_bytes())
        .expect("failed to send host HTTP request");

    let mut buf = [0; 512];
    let len = stream
        .read(&mut buf)
        .expect("failed to read host HTTP response");
    let response = core::str::from_utf8(&buf[..len]).expect("host HTTP response was not utf8");
    assert!(
        response.contains("HTTP/1.1 200 OK"),
        "host HTTP response did not contain status line: {response:?}"
    );
    assert!(
        response.contains(HOST_HTTP_BODY),
        "host HTTP response body mismatch: {response:?}"
    );
    println!("net_loopback: host HTTP client OK");
}

fn test_udp_bind_and_send() {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("failed to bind UDP smoke socket");
    let local = socket
        .local_addr()
        .expect("failed to read UDP smoke socket address");
    assert_ne!(local.port(), 0, "UDP socket did not allocate a port");

    let sent = socket
        .send_to(
            b"arceos-test-suit udp smoke",
            SocketAddr::from(([10, 0, 2, 2], 9)),
        )
        .expect("failed to send UDP smoke datagram");
    assert_eq!(sent, b"arceos-test-suit udp smoke".len());
    println!("net_loopback: UDP smoke socket {local}");
}

fn test_fixed_udp_server_bind() {
    let socket =
        UdpSocket::bind(("0.0.0.0", SERVER_SMOKE_PORT)).expect("failed to bind UDP server port");
    let addr = socket
        .local_addr()
        .expect("failed to read fixed UDP socket address");
    assert_eq!(addr.port(), SERVER_SMOKE_PORT);
    println!("net_loopback: fixed UDP server socket {addr}");
}
