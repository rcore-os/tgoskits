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

#[test]
fn raw_cargo_target_dir_before_rustc_args_is_rejected() {
    let cargo = Cargo {
        package: "kernel".to_string(),
        args: vec!["--target-dir=other".to_string()],
        ..Default::default()
    };

    let error = reject_raw_target_dir_args(&cargo).unwrap_err();
    assert!(error.to_string().contains("raw `--target-dir`"));
}

#[test]
fn rustc_target_dir_argument_after_separator_is_allowed() {
    let cargo = Cargo {
        package: "kernel".to_string(),
        args: vec!["--".to_string(), "--target-dir=guest-value".to_string()],
        ..Default::default()
    };

    reject_raw_target_dir_args(&cargo).unwrap();
}
