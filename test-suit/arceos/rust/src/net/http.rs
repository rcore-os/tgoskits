use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    os::fd::AsRawFd,
};

#[cfg(target_arch = "x86_64")]
const HOST_FIXTURE: &str = "10.0.2.2:18180";
#[cfg(target_arch = "aarch64")]
const HOST_FIXTURE: &str = "10.0.2.2:18181";
#[cfg(target_arch = "riscv64")]
const HOST_FIXTURE: &str = "10.0.2.2:18182";
#[cfg(target_arch = "loongarch64")]
const HOST_FIXTURE: &str = "10.0.2.2:18183";
const HOST_HTTP_BODY: &str = "ArceOS rust test suite host fixture\n";
const HTTP_REQUEST: &str = "GET / HTTP/1.1\r\nHost: axbuild.local\r\nAccept: */*\r\n\r\n";

pub fn run() -> crate::TestResult {
    test_host_http_client();
    test_udp_round_trip();
    Ok(())
}

fn test_host_http_client() {
    let mut stream = TcpStream::connect(HOST_FIXTURE).expect("failed to connect host HTTP fixture");
    stream
        .write_all(HTTP_REQUEST.as_bytes())
        .expect("failed to send host HTTP request");

    let mut buf = [0; 512];
    let mut len = 0;
    while len < buf.len() {
        let read_len = stream
            .read(&mut buf[len..])
            .expect("failed to read host HTTP response");
        if read_len == 0 {
            break;
        }
        len += read_len;
    }
    assert_ne!(len, 0, "host HTTP response was empty");
    let response = core::str::from_utf8(&buf[..len]).expect("host HTTP response was not utf8");
    assert!(
        response.contains("HTTP/1.1 200 OK"),
        "host HTTP response did not contain status line: {response:?}"
    );
    assert!(
        response.contains(HOST_HTTP_BODY),
        "host HTTP response body mismatch: {response:?}"
    );
    println!("net_http: host HTTP client OK");
}

fn test_udp_round_trip() {
    let peer: SocketAddr = HOST_FIXTURE
        .parse()
        .expect("invalid host UDP fixture address");
    for bind in ["0.0.0.0:0", "0.0.0.0:5555"] {
        let socket = UdpSocket::bind(bind).expect("failed to bind UDP socket");
        let local = socket
            .local_addr()
            .expect("failed to read bound UDP address");
        assert_ne!(local.port(), 0, "UDP bind did not allocate a port");
        if bind == "0.0.0.0:5555" {
            assert_eq!(
                local.port(),
                5555,
                "UDP fixed-port bind changed the requested port"
            );
        }
        socket
            .set_nonblocking(true)
            .expect("failed to make UDP socket nonblocking");
        for payload in [
            b"ArceOS UDP request\0\xff".as_slice(),
            b"second datagram".as_slice(),
        ] {
            println!(
                "NET_UDP_BEGIN local={local} peer={peer} bytes={}",
                payload.len()
            );
            wait_udp_ready(&socket, libc::POLLOUT);
            assert_eq!(
                socket
                    .send_to(payload, peer)
                    .expect("failed to send UDP datagram"),
                payload.len(),
                "UDP send returned the wrong length"
            );
            wait_udp_ready(&socket, libc::POLLIN);
            let mut received = [0; 64];
            let (len, source) = socket
                .recv_from(&mut received)
                .expect("UDP echo timed out or receive failed");
            assert_eq!(source, peer, "UDP datagram came from an unexpected peer");
            assert_eq!(&received[..len], payload, "UDP echo payload mismatch");
        }
        println!("NET_UDP_ROUND_TRIP_OK local={local} datagrams=2");
    }
}

fn wait_udp_ready(socket: &UdpSocket, events: libc::c_short) {
    let mut descriptor = libc::pollfd {
        fd: socket.as_raw_fd(),
        events,
        revents: 0,
    };
    // SAFETY: descriptor is one initialized, exclusively borrowed pollfd. The
    // socket stays alive for this synchronous call, and poll retains no pointer.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 3000) };
    assert_eq!(
        ready, 1,
        "UDP readiness timed out or poll failed: events={events}"
    );
    assert_eq!(
        descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL),
        0,
        "UDP socket reported an error: revents={}",
        descriptor.revents
    );
    assert_ne!(
        descriptor.revents & events,
        0,
        "UDP poll did not report the requested event"
    );
}
