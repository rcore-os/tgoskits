use super::*;

mod arceos;
mod axvisor;
mod board_request;
mod common;
mod snapshot;
mod starry;
mod workspace;

#[test]
fn cargo_bin_path_replaces_elf_extension() {
    assert_eq!(
        cargo_bin_path_for_elf(Path::new("/workspace/target/release/kernel")),
        PathBuf::from("/workspace/target/release/kernel.bin")
    );
    assert_eq!(
        cargo_bin_path_for_elf(Path::new("/workspace/target/release/kernel.elf")),
        PathBuf::from("/workspace/target/release/kernel.bin")
    );
}
