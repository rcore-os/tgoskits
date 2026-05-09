use std::{env, process};

fn main() {
    println!("Hello from Rust on StarryOS!");
    println!("PID: {}", process::id());
    println!("Args: {:?}", env::args().collect::<Vec<_>>());

    // Basic sanity checks
    assert_eq!(1 + 1, 2, "math is broken");
    assert!(process::id() > 0, "getpid failed");

    println!("TEST PASSED");
}
