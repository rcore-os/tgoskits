#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[cfg(feature = "ax-std")]
use ax_std::println;

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, world!");
}
