use std::{
    io::{Read, Write},
    net::TcpStream,
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

pub fn run() -> crate::TestResult {
    test_host_http_client();
    Ok(())
}

fn test_host_http_client() {
    let mut stream = TcpStream::connect(HOST_HTTP).expect("failed to connect host HTTP fixture");
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
