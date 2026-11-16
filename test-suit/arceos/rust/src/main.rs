#[cfg(feature = "ax-std")]
mod runner;

#[cfg(feature = "ax-std")]
fn main() {
    runner::run();
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!(
        "arceos-test-suit requires a test feature; select a test or batch with cargo xtask arceos \
         test qemu"
    );
}
