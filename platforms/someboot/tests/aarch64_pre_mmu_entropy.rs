use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const TARGET: &str = "aarch64-unknown-none-softfloat";
const CAPTURE: &str = "someboot::entropy::capture";
const SYNCHRONIZED_INIT_CALL: &str =
    "<kernutil::staticcell::StaticCell<core::option::Option<someboot::entropy::BootEntropy>>>::init>";
const SINGLE_CORE_INIT: &str =
    "<kernutil::staticcell::StaticCell<core::option::Option<someboot::entropy::BootEntropy>>>::init_single_core";

#[test]
fn boot_entropy_publication_avoids_exclusive_atomics_before_mmu_enable() {
    let temporary_directory = TemporaryDirectory::new();
    let archive = build_aarch64_archive(temporary_directory.path());
    let disassembly = run_output(
        Command::new("rust-objdump")
            .args(["-dr", "-C"])
            .arg(&archive),
        "disassemble the AArch64 someboot archive",
    );
    let capture = function_disassembly(&disassembly, CAPTURE);
    assert!(
        capture.contains(SINGLE_CORE_INIT),
        "pre-MMU entropy publication must use single-core initialization:\n{capture}"
    );
    assert!(
        !capture.contains(SYNCHRONIZED_INIT_CALL),
        "pre-MMU entropy publication must not call synchronized StaticCell::init:\n{capture}"
    );
}

fn build_aarch64_archive(target_directory: &Path) -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_manifest = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("someboot must be two levels below the workspace root")
        .join("Cargo.toml");
    run_output(
        Command::new(cargo)
            .args([
                "build",
                "--package",
                "someboot",
                "--lib",
                "--target",
                TARGET,
                "--target-dir",
            ])
            .arg(target_directory)
            .args(["--manifest-path"])
            .arg(workspace_manifest),
        "compile someboot for AArch64",
    );

    let dependency_directory = target_directory.join(TARGET).join("debug").join("deps");
    let mut archives = fs::read_dir(&dependency_directory)
        .expect("read the isolated AArch64 dependency directory")
        .map(|entry| entry.expect("read an archive directory entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libsomeboot-") && name.ends_with(".rlib"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        archives.len(),
        1,
        "the isolated target directory must contain one someboot archive"
    );
    archives.pop().expect("the someboot archive must exist")
}

fn function_disassembly<'a>(disassembly: &'a str, symbol: &str) -> &'a str {
    let header = format!("<{symbol}>:");
    let start = disassembly
        .find(&header)
        .unwrap_or_else(|| panic!("missing disassembly for {symbol}"));
    let body = &disassembly[start + header.len()..];
    body.split("\n\n").next().unwrap_or(body)
}

fn run_output(command: &mut Command, description: &str) -> String {
    let output = command.output().unwrap_or_else(|error| {
        panic!("failed to {description}: {error}");
    });
    assert!(
        output.status.success(),
        "failed to {description}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("command output must be UTF-8")
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "someboot-aarch64-pre-mmu-entropy-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary AArch64 target directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove temporary AArch64 target directory");
    }
}
