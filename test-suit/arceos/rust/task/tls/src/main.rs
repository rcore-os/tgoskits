#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]
#![feature(thread_local)]
#![allow(unused_unsafe)]

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

use std::{ptr::addr_of, str::from_utf8_unchecked, thread, vec::Vec};

#[thread_local]
static mut BOOL: bool = true;

#[thread_local]
static mut U8: u8 = 0xAA;

#[thread_local]
static mut U16: u16 = 0xcafe;

#[thread_local]
static mut U32: u32 = 0xdeadbeed;

#[thread_local]
static mut U64: u64 = 0xa2ce05_a2ce05;

#[thread_local]
static mut STR: [u8; 13] = *b"Hello, world!";

const STR_LEN: usize = 13;

macro_rules! get {
    ($var:expr) => {
        unsafe { $var }
    };
}

macro_rules! set {
    ($var:expr, $value:expr) => {
        unsafe { $var = $value }
    };
}

macro_rules! add {
    ($var:expr, $value:expr) => {
        unsafe { $var += $value }
    };
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Running TLS tests...");

    println!(
        "main: {} {:#x} {:#x} {:#x} {:#x} {}",
        get!(BOOL),
        get!(U8),
        get!(U16),
        get!(U32),
        get!(U64),
        get!(from_utf8_unchecked(&*addr_of!(STR)))
    );
    assert!(get!(BOOL));
    assert_eq!(get!(U8), 0xAA);
    assert_eq!(get!(U16), 0xcafe);
    assert_eq!(get!(U32), 0xdeadbeed);
    assert_eq!(get!(U64), 0xa2ce05_a2ce05);
    assert_eq!(get!(&*addr_of!(STR)), b"Hello, world!");

    let mut tasks = Vec::new();
    for i in 1..=10 {
        tasks.push(thread::spawn(move || {
            set!(BOOL, i % 2 == 0);
            add!(U8, i as u8);
            add!(U16, i as u16);
            add!(U32, i as u32);
            add!(U64, i as u64);
            set!(STR[5], 48 + i as u8);

            thread::yield_now();

            println!(
                "{}: {} {:#x} {:#x} {:#x} {:#x} {}",
                i,
                get!(BOOL),
                get!(U8),
                get!(U16),
                get!(U32),
                get!(U64),
                get!(from_utf8_unchecked(&*addr_of!(STR)))
            );
            assert_eq!(get!(BOOL), i % 2 == 0);
            assert_eq!(get!(U8), 0xAA + i as u8);
            assert_eq!(get!(U16), 0xcafe + i as u16);
            assert_eq!(get!(U32), 0xdeadbeed + i as u32);
            assert_eq!(get!(U64), 0xa2ce05_a2ce05 + i as u64);
            assert_eq!(get!(STR[5]), 48 + i as u8);
            assert_eq!(STR_LEN, 13);
        }));
    }

    tasks.into_iter().for_each(|t| t.join().unwrap());

    // TLS of main thread must not have been changed by the other thread.
    assert!(get!(BOOL));
    assert_eq!(get!(U8), 0xAA);
    assert_eq!(get!(U16), 0xcafe);
    assert_eq!(get!(U32), 0xdeadbeed);
    assert_eq!(get!(U64), 0xa2ce05_a2ce05);
    assert_eq!(get!(&*addr_of!(STR)), b"Hello, world!");

    println!("TLS tests run OK!");
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
