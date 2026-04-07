#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use std::{
    io::{self, prelude::*},
    net::{TcpListener, TcpStream},
    thread,
    vec::Vec,
};

const LOCAL_IP: &str = "0.0.0.0";
const LOCAL_PORT: u16 = 5555;

fn reverse(buf: &[u8]) -> Vec<u8> {
    let mut lines = buf
        .split(|&b| b == b'\n')
        .map(Vec::from)
        .collect::<Vec<_>>();
    for line in lines.iter_mut() {
        line.reverse();
    }
    lines.join(&b'\n')
}

fn echo_server(mut stream: TcpStream) -> io::Result<()> {
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        stream.write_all(reverse(&buf[..n]).as_slice())?;
    }
}

fn accept_loop() -> io::Result<()> {
    let listener = TcpListener::bind((LOCAL_IP, LOCAL_PORT))?;
    println!("listen on: {}", listener.local_addr().unwrap());

    let mut i = 0;
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                println!("new client {}: {}", i, addr);
                thread::spawn(move || match echo_server(stream) {
                    Err(e) => println!("client connection error: {:?}", e),
                    Ok(()) => println!("client {} closed successfully", i),
                });
            }
            Err(e) => return Err(e),
        }
        i += 1;
    }
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, echo server!");
    accept_loop().expect("test echo server failed");
}
