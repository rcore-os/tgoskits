#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(any(not(target_os = "none"), feature = "ax-std"))]
macro_rules! app {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(any(not(target_os = "none"), feature = "ax-std")))]
macro_rules! app {
    ($($item:item)*) => {};
}

app! {

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

#[cfg(feature = "ax-std")]
fn not_found_error() -> io::Error {
    io::Error::from(ax_io::ErrorKind::NotFound)
}

#[cfg(not(feature = "ax-std"))]
fn not_found_error() -> io::Error {
    io::Error::from(io::ErrorKind::NotFound)
}

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
    Err(not_found_error())
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, simple http client!");
    client().expect("test http client failed");
    std::process::exit(0);
}

}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
