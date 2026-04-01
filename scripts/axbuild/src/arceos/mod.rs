use std::{
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};

use crate::{
    command_flow::{self, SnapshotPersistence},
    context::{AppContext, BuildCliArgs, QemuRunConfig, ResolvedBuildRequest},
    process::ProcessExt,
    test_qemu,
};

/// Prepare any runtime assets (disk images, etc.) required by `package`.
fn ensure_package_runtime_assets(package: &str) -> anyhow::Result<()> {
    match package {
        "arceos-fs-shell" => ensure_fat32_image(
            "test-suit/arceos/rust/fs/shell/disk.img",
            "64M",
            "generating disk.img for arceos-fs-shell",
        ),
        _ => Ok(()),
    }
}

/// Create a FAT32 disk image at `path` with the given `size` if it does not
/// already exist.
fn ensure_fat32_image(path: &str, size: &str, msg: &str) -> anyhow::Result<()> {
    let image = std::path::Path::new(path);
    if image.exists() {
        return Ok(());
    }
    println!("{msg} ...");
    if let Some(parent) = image.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let ran = |cmd: &mut StdCommand| -> anyhow::Result<()> {
        let name = cmd.get_program().to_string_lossy().to_string();
        cmd.status()
            .with_context(|| format!("failed to run `{name}`"))?
            .success()
            .then_some(())
            .ok_or_else(|| anyhow::anyhow!("`{name}` exited with non-zero status"))
    };
    ran(StdCommand::new("truncate").args(["-s", size]).arg(image))?;
    ran(StdCommand::new("mkfs.fat")
        .args(["-F", "32"])
        .arg(image)
        .stdout(std::process::Stdio::null()))?;
    println!("{msg} ... done");
    Ok(())
}

pub mod build;
mod c_test_cargo_config;

// ---------------------------------------------------------------------------
// C test definitions
// ---------------------------------------------------------------------------

/// A discovered C test under `test-suit/arceos/c/`.
struct CTestDef {
    name: String,
    dir: PathBuf,
    features: Vec<String>,
}

/// Known C test directories (relative to `test-suit/arceos/c/`).
const C_TEST_NAMES: &[&str] = &[
    "helloworld",
    "memtest",
    "httpclient",
    "pthread/basic",
    "pthread/parallel",
    "pthread/pipe",
    "pthread/sleep",
];

/// Discover available C tests by checking which directories exist.
fn discover_c_tests(c_test_root: &Path) -> Vec<CTestDef> {
    let mut tests = Vec::new();
    for name in C_TEST_NAMES {
        let dir = c_test_root.join(name);
        // A C test is valid if it contains at least one .c file
        let has_c = std::fs::read_dir(&dir)
            .ok()
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "c"))
            })
            .unwrap_or(false);
        if has_c {
            let features = load_features_txt(&dir.join("features.txt"));
            tests.push(CTestDef {
                name: name.to_string(),
                dir,
                features,
            });
        }
    }
    tests
}

/// Load features from a `features.txt` file (one feature per line).
fn load_features_txt(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Extract architecture short name from a target triple (e.g. "x86_64-unknown-none" → "x86_64").
fn arch_from_target(target: &str) -> &str {
    if target.starts_with("x86_64") {
        "x86_64"
    } else if target.starts_with("aarch64") {
        "aarch64"
    } else if target.starts_with("riscv64") {
        "riscv64"
    } else if target.starts_with("loongarch64") {
        "loongarch64"
    } else {
        "unknown"
    }
}

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// ArceOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build ArceOS application
    Build(ArgsBuild),
    /// Build and run ArceOS application in QEMU
    Qemu(ArgsQemu),
    /// Build and run ArceOS application with U-Boot
    Uboot(ArgsUboot),
    /// Run ArceOS test suites
    Test(ArgsTest),
}

#[derive(Args)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(short, long)]
    pub package: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,
    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run ArceOS QEMU test suites (Rust + C by default)
    Qemu(ArgsTestQemu),
    /// Reserved ArceOS U-Boot test suite entrypoint
    Uboot(ArgsTestUboot),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(long)]
    pub target: String,
    /// Only run Rust tests
    #[arg(long, conflicts_with = "only_c")]
    pub only_rust: bool,
    /// Only run C tests
    #[arg(long, conflicts_with = "only_rust")]
    pub only_c: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestUboot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QemuTestFlow {
    Rust,
    C,
}

// ---------------------------------------------------------------------------
// ArceOS executor
// ---------------------------------------------------------------------------

pub struct ArceOS {
    app: AppContext,
}

impl From<&ArgsBuild> for BuildCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            package: args.package.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
        }
    }
}

impl ArceOS {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Test(args) => self.test(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        ensure_package_runtime_assets(&request.package)?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        ensure_package_runtime_assets(&request.package)?;
        self.run_qemu_request(request).await
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            None,
            args.uboot_config,
            SnapshotPersistence::Store,
        )?;
        ensure_package_runtime_assets(&request.package)?;
        self.run_uboot_request(request).await
    }

    // ---- test dispatch ----

    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        match args.command {
            TestCommand::Qemu(args) => {
                for flow in planned_qemu_test_flows(&args) {
                    match flow {
                        QemuTestFlow::Rust => self.test_rust_qemu(args.clone()).await?,
                        QemuTestFlow::C => self.test_c_qemu(args.clone()).await?,
                    }
                }
                Ok(())
            }
            TestCommand::Uboot(args) => self.test_uboot(args).await,
        }
    }

    // ---- Rust QEMU tests ----

    async fn test_rust_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        let target = test_qemu::validate_arceos_target(&args.target)?;
        let mut failed = Vec::new();

        println!(
            "running arceos qemu tests for {} package(s) on target: {}",
            test_qemu::ARCEOS_TEST_PACKAGES.len(),
            target
        );

        for (index, package) in test_qemu::ARCEOS_TEST_PACKAGES.iter().enumerate() {
            println!(
                "[{}/{}] arceos qemu {}",
                index + 1,
                test_qemu::ARCEOS_TEST_PACKAGES.len(),
                package
            );
            ensure_package_runtime_assets(package)?;
            let request = self.prepare_request(
                Self::test_build_args(package, target),
                None,
                None,
                SnapshotPersistence::Discard,
            )?;
            match self
                .run_qemu_request(request)
                .await
                .with_context(|| format!("arceos qemu test failed for package `{package}`"))
            {
                Ok(()) => println!("ok: {}", package),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", package, err);
                    failed.push((*package).to_string());
                }
            }
        }

        test_qemu::finalize_qemu_test_run("arceos", &failed)
    }

    // ---- C QEMU tests ----

    async fn test_c_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        run_c_qemu_tests_with_hooks(
            self.app.workspace_root(),
            &args.target,
            c_test_cargo_config::prepare_c_test_cargo_config,
            run_single_c_qemu_test,
        )
    }

    // ---- U-Boot tests (placeholder) ----

    async fn test_uboot(&mut self, _args: ArgsTestUboot) -> anyhow::Result<()> {
        test_qemu::unsupported_uboot_test_command("arceos")
    }

    // ---- internal helpers ----

    fn prepare_request(
        &self,
        args: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedBuildRequest> {
        command_flow::resolve_request(
            persistence,
            || {
                self.app
                    .prepare_arceos_request(args, qemu_config, uboot_config)
            },
            |snapshot| self.app.store_arceos_snapshot(snapshot),
        )
    }

    fn test_build_args(package: &str, target: &str) -> BuildCliArgs {
        BuildCliArgs {
            config: None,
            package: Some(package.to_string()),
            target: Some(target.to_string()),
            plat_dyn: None,
        }
    }

    fn qemu_run_config(request: &ResolvedBuildRequest) -> anyhow::Result<QemuRunConfig> {
        Ok(QemuRunConfig {
            qemu_config: request.qemu_config.clone(),
            ..Default::default()
        })
    }

    async fn run_qemu_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_qemu(
            &mut self.app,
            request,
            build::load_cargo_config,
            Self::qemu_run_config,
        )
        .await
    }

    async fn run_build_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_uboot(&mut self.app, request, build::load_cargo_config).await
    }
}

fn planned_qemu_test_flows(args: &ArgsTestQemu) -> &'static [QemuTestFlow] {
    if args.only_c {
        &[QemuTestFlow::C]
    } else if args.only_rust {
        &[QemuTestFlow::Rust]
    } else {
        &[QemuTestFlow::Rust, QemuTestFlow::C]
    }
}

fn run_c_qemu_tests_with_hooks<PrepareConfig, RunTest>(
    workspace_root: &Path,
    target: &str,
    mut prepare_config: PrepareConfig,
    mut run_test: RunTest,
) -> anyhow::Result<()>
where
    PrepareConfig: FnMut(&Path) -> anyhow::Result<PathBuf>,
    RunTest: FnMut(&Path, &Path, &str, &str) -> anyhow::Result<()>,
{
    let target = test_qemu::validate_arceos_target(target)?;
    let arch = arch_from_target(target);
    let arceos_dir = workspace_root.join("os/arceos");
    let c_test_root = workspace_root.join("test-suit/arceos/c");

    if !arceos_dir.join("Makefile").exists() {
        bail!(
            "arceos Makefile not found at {}, required for C test builds",
            arceos_dir.display()
        );
    }

    let c_tests = discover_c_tests(&c_test_root);
    if c_tests.is_empty() {
        println!("no C tests found in {}", c_test_root.display());
        return Ok(());
    }

    let config_path = prepare_config(workspace_root)?;
    println!(
        "prepared ArceOS C-test cargo config: {}",
        config_path.display()
    );

    let mut failed = Vec::new();
    println!(
        "running arceos C qemu tests for {} test(s) on target: {} (arch: {})",
        c_tests.len(),
        target,
        arch
    );

    for (index, c_test) in c_tests.iter().enumerate() {
        println!(
            "[{}/{}] arceos c qemu {}",
            index + 1,
            c_tests.len(),
            c_test.name
        );

        let app_path = match c_test.dir.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                eprintln!("failed: c/{}: cannot resolve path: {err:#}", c_test.name);
                failed.push(c_test.name.clone());
                continue;
            }
        };

        let features_str = c_test.features.join(",");
        let result = run_test(&arceos_dir, &app_path, arch, &features_str)
            .with_context(|| format!("c test `{}` failed", c_test.name));
        match result {
            Ok(()) => println!("ok: c/{}", c_test.name),
            Err(err) => {
                eprintln!("failed: c/{}: {:#}", c_test.name, err);
                failed.push(c_test.name.clone());
            }
        }
    }

    test_qemu::finalize_qemu_test_run("arceos c", &failed)
}

fn run_single_c_qemu_test(
    arceos_dir: &Path,
    app_path: &Path,
    arch: &str,
    features: &str,
) -> anyhow::Result<()> {
    StdCommand::new("make")
        .current_dir(arceos_dir)
        .arg(format!("A={}", app_path.display()))
        .arg(format!("ARCH={}", arch))
        .arg(format!("FEATURES={}", features))
        .arg("ACCEL=n")
        .arg("run")
        .exec()
}

impl Default for ArceOS {
    fn default() -> Self {
        Self::new().expect("failed to initialize ArceOS")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use clap::Parser;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn command_parses_test_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli =
            Cli::try_parse_from(["arceos", "test", "qemu", "--target", "x86_64-unknown-none"])
                .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target, "x86_64-unknown-none");
                    assert!(!args.only_rust);
                    assert!(!args.only_c);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_only_rust() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-rust",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target, "x86_64-unknown-none");
                    assert!(args.only_rust);
                    assert!(!args.only_c);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_only_c() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-c",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target, "x86_64-unknown-none");
                    assert!(!args.only_rust);
                    assert!(args.only_c);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_rejects_both_only_flags() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let result = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-rust",
            "--only-c",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn command_parses_test_uboot() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["arceos", "test", "uboot"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(_) => {}
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn arch_from_target_extracts_correct_arch() {
        assert_eq!(arch_from_target("x86_64-unknown-none"), "x86_64");
        assert_eq!(
            arch_from_target("aarch64-unknown-none-softfloat"),
            "aarch64"
        );
        assert_eq!(arch_from_target("riscv64gc-unknown-none-elf"), "riscv64");
        assert_eq!(
            arch_from_target("loongarch64-unknown-none-softfloat"),
            "loongarch64"
        );
    }

    #[test]
    fn load_features_txt_parses_correctly() {
        let dir = std::env::temp_dir().join("axbuild_test_features");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("features.txt"), "alloc\npaging\nnet\n").unwrap();

        let features = load_features_txt(&dir.join("features.txt"));
        assert_eq!(features, vec!["alloc", "paging", "net"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_features_txt_handles_missing_file() {
        let features = load_features_txt(Path::new("/nonexistent/features.txt"));
        assert!(features.is_empty());
    }

    #[test]
    fn discover_c_tests_finds_valid_tests() {
        let dir = std::env::temp_dir().join("axbuild_test_c");
        std::fs::create_dir_all(dir.join("helloworld")).unwrap();
        std::fs::write(dir.join("helloworld/main.c"), "int main() { return 0; }\n").unwrap();
        std::fs::create_dir_all(dir.join("empty")).unwrap();

        let tests = discover_c_tests(&dir);
        // Only "helloworld" should be found, "empty" is not in C_TEST_NAMES
        assert!(tests.iter().any(|t| t.name == "helloworld"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn planned_qemu_test_flows_default_runs_rust_then_c() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            target: "x86_64-unknown-none".to_string(),
            only_rust: false,
            only_c: false,
        });

        assert_eq!(flows, &[QemuTestFlow::Rust, QemuTestFlow::C]);
    }

    #[test]
    fn planned_qemu_test_flows_only_rust_skips_c() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            target: "x86_64-unknown-none".to_string(),
            only_rust: true,
            only_c: false,
        });

        assert_eq!(flows, &[QemuTestFlow::Rust]);
    }

    #[test]
    fn planned_qemu_test_flows_only_c_skips_rust() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            target: "x86_64-unknown-none".to_string(),
            only_rust: false,
            only_c: true,
        });

        assert_eq!(flows, &[QemuTestFlow::C]);
    }

    #[test]
    fn run_c_qemu_tests_with_hooks_prepares_config_once_before_running_tests() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let arceos_dir = workspace_root.join("os/arceos");
        let c_root = workspace_root.join("test-suit/arceos/c");

        std::fs::create_dir_all(&arceos_dir).unwrap();
        std::fs::create_dir_all(c_root.join("helloworld")).unwrap();
        std::fs::create_dir_all(c_root.join("memtest")).unwrap();
        std::fs::write(arceos_dir.join("Makefile"), "run:\n\t@true\n").unwrap();
        std::fs::write(
            c_root.join("helloworld/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            c_root.join("memtest/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));

        let prepare_events = events.clone();
        let run_events = events.clone();
        run_c_qemu_tests_with_hooks(
            workspace_root,
            "x86_64-unknown-none",
            move |root| {
                prepare_events
                    .lock()
                    .unwrap()
                    .push(format!("prepare:{}", root.display()));
                Ok(root.join("os/arceos/.cargo/config.toml"))
            },
            move |_arceos_dir, app_path, _arch, _features| {
                run_events.lock().unwrap().push(format!(
                    "run:{}",
                    app_path.file_name().unwrap().to_string_lossy()
                ));
                Ok(())
            },
        )
        .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events[0], format!("prepare:{}", workspace_root.display()));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("prepare:"))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("run:"))
                .count(),
            2
        );
    }
}
