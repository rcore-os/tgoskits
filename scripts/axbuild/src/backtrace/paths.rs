use std::path::{Path, PathBuf};

/// Resolved ELF path for an ArceOS Rust package built via the workspace `target/` dir.
pub(crate) fn arceos_rust_elf_path(
    workspace_root: &Path,
    target: &str,
    package: &str,
    debug: bool,
) -> PathBuf {
    let profile = if debug { "debug" } else { "release" };
    workspace_root
        .join("target")
        .join(target)
        .join(profile)
        .join(package)
}

/// Resolved ELF path for an ArceOS std test package built via the workspace `target/` dir.
pub(crate) fn std_test_elf_path(
    workspace_root: &Path,
    target: &str,
    package: &str,
    debug: bool,
) -> PathBuf {
    arceos_rust_elf_path(workspace_root, std_test_target_dir(target), package, debug)
}

fn std_test_target_dir(target: &str) -> &str {
    if target.starts_with("x86_64-") {
        "x86_64-unknown-linux-musl"
    } else if target.starts_with("aarch64-") {
        "aarch64-unknown-linux-musl"
    } else if target.starts_with("riscv64") {
        "riscv64gc-unknown-linux-musl"
    } else if target.starts_with("loongarch64-") {
        "loongarch64-unknown-linux-musl"
    } else {
        target
    }
}
