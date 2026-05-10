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
    io::{self, prelude::*},
    net::{TcpStream, ToSocketAddrs},
};

#[cfg(feature = "dns")]
const DEST: &str = "ident.me:80";
#[cfg(not(feature = "dns"))]
const DEST: &str = "65.108.151.63:80";

const REQUEST: &str = "\
GET / HTTP/1.1\r\nHost: ident.me\r\nAccept: */*\r\n\r\n";

fn client() -> io::Result<()> {
    for addr in DEST.to_socket_addrs()? {
        println!("dest: {DEST} ({addr})");
    }

    let mut stream = TcpStream::connect(DEST)?;
    stream.write_all(REQUEST.as_bytes())?;
    let mut buf = [0; 2048];
    let n = stream.read(&mut buf)?;
    let response = core::str::from_utf8(&buf[..n]).unwrap();
    println!("{response}"); // longer response need to handle tcp package problems.
    Ok(())
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, simple http client!");
    client().expect("test http client failed");
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
