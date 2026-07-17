#[test]
fn synchronous_spi_block_backend_is_not_part_of_the_public_crate() {
    let manifest = include_str!("../Cargo.toml");
    let library = include_str!("../src/lib.rs");

    assert!(
        !manifest.lines().any(|line| line.trim() == "spi = []"),
        "a synchronous SPI feature bypasses the IRQ-only block runtime"
    );
    assert!(
        !library.contains("pub mod spi"),
        "normal hardware block I/O must not expose a busy-polling backend"
    );
}

#[test]
fn rdif_shared_core_has_only_one_shot_non_blocking_acquisition() {
    let shared_core = include_str!("../src/rdif/shared_core.rs");

    assert!(shared_core.contains("fn try_borrow_mut("));
    assert!(shared_core.contains("compare_exchange(false, true"));
    assert!(!shared_core.contains("spin_loop"));
    assert!(!shared_core.contains("loop {"));
    assert!(!shared_core.contains("fn enter("));
}
