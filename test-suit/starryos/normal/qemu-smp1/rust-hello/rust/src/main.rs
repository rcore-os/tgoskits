use std::{
    env,
    io::{Read, Write},
    process,
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};

fn main() {
    println!("Hello from Rust on StarryOS!");
    println!("PID: {}", process::id());
    println!("Args: {:?}", env::args().collect::<Vec<_>>());

    // Basic sanity checks
    assert_eq!(1 + 1, 2, "math is broken");
    assert!(process::id() > 0, "getpid failed");

    // Exercise zlib (installed by prebuild.sh) via flate2.
    let input = b"Hello from zlib on StarryOS!";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input).expect("compress write failed");
    let compressed = encoder.finish().expect("compress finish failed");
    assert!(!compressed.is_empty(), "compressed output is empty");

    let mut decoder = GzDecoder::new(compressed.as_slice());
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .expect("decompress failed");
    assert_eq!(decompressed, input, "round-trip mismatch");

    println!(
        "zlib round-trip ok ({} -> {} bytes)",
        input.len(),
        compressed.len()
    );
    println!("TEST PASSED");
}
