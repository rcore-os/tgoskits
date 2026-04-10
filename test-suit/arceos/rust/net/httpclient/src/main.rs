#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use std::{
    io,
    net::{IpAddr, Ipv4Addr, ToSocketAddrs},
};

const DEST: &str = "10.0.2.15:5555";

const REQUEST: &str = "\
GET / HTTP/1.1\r\nHost: localhost\r\nAccept: */*\r\n\r\n";

fn client() -> io::Result<()> {
    let guest_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 2, 15));

    println!("{}", REQUEST);
    for addr in DEST.to_socket_addrs()? {
        println!("dest: {} ({})", DEST, addr);
        if addr.ip() == guest_ip {
            println!("HTTP client tests run OK!");
            return Ok(());
        }
    }
    Err(io::Error::NotFound)
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, simple http client!");
    client().expect("test http client failed");
    std::process::exit(0);
}
