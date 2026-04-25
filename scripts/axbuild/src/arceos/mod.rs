use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Output},
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::build::config::Cargo;
use regex::Regex;

use crate::{
    command_flow::{self, SnapshotPersistence},
    context::{AppContext, BuildCliArgs, ResolvedBuildRequest},
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
pub mod rootfs;

// ---------------------------------------------------------------------------
// C test definitions
// ---------------------------------------------------------------------------

/// A discovered C test under `test-suit/arceos/c/`.
struct CTestDef {
    name: String,
    dir: PathBuf,
    features: Vec<String>,
    invocations: Vec<CTestInvocation>,
}

/// One `test_one "..." "..."` entry from a C test `test_cmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CTestInvocation {
    make_vars: Vec<(String, String)>,
    expect_output: Option<PathBuf>,
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
fn discover_c_tests(c_test_root: &Path) -> anyhow::Result<Vec<CTestDef>> {
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
            let invocations = load_c_test_invocations(&dir.join("test_cmd"))?;
            tests.push(CTestDef {
                name: name.to_string(),
                dir,
                features,
                invocations,
            });
        }
    }
    Ok(tests)
}

/// Load features from a `features.txt` file (one feature per line).
fn load_features_txt(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn load_c_test_invocations(path: &Path) -> anyhow::Result<Vec<CTestInvocation>> {
    if !path.exists() {
        return Ok(vec![CTestInvocation {
            make_vars: Vec::new(),
            expect_output: None,
        }]);
    }

    let test_one_regex = Regex::new(r#"^test_one\s+"([^"]*)"\s+"([^"]+)"\s*$"#)
        .expect("invalid C test command regex");
    let mut invocations = Vec::new();

    for (line_no, raw_line) in fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .lines()
        .enumerate()
    {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line == "rm -f $APP/*.o" {
            continue;
        }

        let captures = test_one_regex.captures(line).ok_or_else(|| {
            anyhow::anyhow!("unsupported C test command at {}: {}", path.display(), line)
        })?;
        let make_vars = parse_c_test_make_vars(&captures[1]).with_context(|| {
            format!(
                "failed to parse make vars at {}:{}",
                path.display(),
                line_no + 1
            )
        })?;
        invocations.push(CTestInvocation {
            make_vars,
            expect_output: Some(PathBuf::from(&captures[2])),
        });
    }

    if invocations.is_empty() {
        invocations.push(CTestInvocation {
            make_vars: Vec::new(),
            expect_output: None,
        });
    }

    Ok(invocations)
}

fn parse_c_test_make_vars(input: &str) -> anyhow::Result<Vec<(String, String)>> {
    let mut vars = Vec::new();
    for assignment in input.split_whitespace() {
        let (key, value) = assignment
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid make variable assignment `{assignment}`"))?;
        vars.push((key.to_string(), value.to_string()));
    }
    Ok(vars)
}

fn build_c_test_make_args(
    app_path: &Path,
    arch: &str,
    base_features: &[String],
    invocation: &CTestInvocation,
) -> Vec<String> {
    let mut features = base_features.to_vec();
    let mut extra_vars = Vec::<(String, String)>::new();

    for (key, value) in &invocation.make_vars {
        if key == "FEATURES" {
            for feature in value
                .split(',')
                .map(str::trim)
                .filter(|feature| !feature.is_empty())
            {
                if !features.iter().any(|existing| existing == feature) {
                    features.push(feature.to_string());
                }
            }
            continue;
        }

        match extra_vars.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, existing_value)) => *existing_value = value.clone(),
            None => extra_vars.push((key.clone(), value.clone())),
        }
    }

    let mut args = vec![
        format!("A={}", app_path.display()),
        format!("ARCH={}", arch),
        "ACCEL=n".to_string(),
    ];
    if !features.is_empty() {
        args.push(format!("FEATURES={}", features.join(",")));
    }
    args.extend(
        extra_vars
            .into_iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    args
}

fn runtime_output_regex(pattern: &str) -> anyhow::Result<Regex> {
    Regex::new(&translate_bre_to_regex(pattern))
        .with_context(|| format!("invalid expected-output regex `{pattern}`"))
}

fn translate_bre_to_regex(pattern: &str) -> String {
    let mut translated = String::new();
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(next @ ('+' | '?' | '{' | '}' | '(' | ')' | '|')) => {
                    translated.push(next);
                }
                Some(next) => {
                    translated.push('\\');
                    translated.push(next);
                }
                None => translated.push('\\'),
            }
            continue;
        }

        if matches!(ch, '(' | ')' | '|' | '+' | '?' | '{' | '}') {
            translated.push('\\');
        }
        translated.push(ch);
    }

    translated
}

fn normalize_c_test_runtime_output(output: &Output) -> String {
    let ansi_regex =
        Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").expect("invalid ANSI stripping regex");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ansi_regex
        .replace_all(&combined.replace(['\r', '\0', '\u{0007}'], ""), "")
        .into_owned()
}

fn verify_c_test_runtime_output(output: &Output, expected_path: &Path) -> anyhow::Result<()> {
    let normalized = normalize_c_test_runtime_output(output);
    let actual_lines = normalized.lines().collect::<Vec<_>>();
    let expected = fs::read_to_string(expected_path)
        .with_context(|| format!("failed to read {}", expected_path.display()))?;

    for pattern in expected
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let regex = runtime_output_regex(pattern)?;
        if actual_lines.iter().any(|line| regex.is_match(line)) {
            continue;
        }

        let remaining = actual_lines
            .iter()
            .take(40)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "runtime output did not match `{pattern}` from {}. Captured output excerpt:\n{}",
            expected_path.display(),
            remaining
        );
    }

    Ok(())
}

fn c_test_invocation_label(invocation: &CTestInvocation) -> String {
    if invocation.make_vars.is_empty() {
        "default".to_string()
    } else {
        invocation
            .make_vars
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
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
    /// Run ArceOS test suites
    Test(ArgsTest),
    /// Build and run ArceOS application with U-Boot
    Uboot(ArgsUboot),
}

#[derive(Args)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(short, long)]
    pub package: Option<String>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,
    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,

    /// Override the rootfs disk image path (skips auto-download).
    #[arg(long, value_name = "IMAGE")]
    pub rootfs: Option<PathBuf>,
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
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present = "target",
        help = "ArceOS architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present = "arch",
        help = "ArceOS target triple to test"
    )]
    pub target: Option<String>,
    /// Only run the specified Rust test package(s)
    #[arg(short, long, value_name = "PACKAGE", conflicts_with = "only_c")]
    pub package: Vec<String>,
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
            arch: args.arch.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
            smp: args.smp,
            debug: args.debug,
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
        if let Some(rootfs) = args.rootfs {
            let rootfs =
                rootfs::resolve_explicit_rootfs(self.app.workspace_root(), &request.arch, rootfs);
            rootfs::ensure_rootfs_ready(self.app.workspace_root(), &request.arch, &rootfs).await?;
            self.app.set_debug_mode(request.debug)?;
            let cargo = build::load_cargo_config(&request)?;
            let mut qemu = self
                .load_qemu_config(&request, &cargo)
                .await?
                .unwrap_or_default();
            rootfs::patch_qemu_rootfs(&mut qemu, &rootfs);
            self.app
                .qemu(cargo, request.build_info_path, Some(qemu))
                .await
        } else {
            self.run_qemu_request(request).await
        }
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
        let (_arch, target) = test_qemu::parse_arceos_test_target(&args.arch, &args.target)?;
        let packages = select_arceos_test_packages(&args.package)?;
        let mut failed = Vec::new();

        println!(
            "running arceos qemu tests for {} package(s) on target: {}",
            packages.len(),
            target
        );

        for (index, package) in packages.iter().enumerate() {
            println!("[{}/{}] arceos qemu {}", index + 1, packages.len(), package);
            ensure_package_runtime_assets(package)?;
            let qemu_config = Some(Self::resolve_test_qemu_config(package, &target)?);
            let request = self.prepare_request(
                Self::test_build_args(package, &target),
                qemu_config,
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
        let (_arch, target) = test_qemu::parse_arceos_test_target(&args.arch, &args.target)?;
        run_c_qemu_tests_with_hooks(
            self.app.workspace_root(),
            &target,
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
        let (request, snapshot) =
            self.app
                .prepare_arceos_request(args, qemu_config, uboot_config)?;
        if matches!(persistence, SnapshotPersistence::Store) {
            self.app.store_arceos_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn test_build_args(package: &str, target: &str) -> BuildCliArgs {
        BuildCliArgs {
            config: None,
            package: Some(package.to_string()),
            arch: None,
            target: Some(target.to_string()),
            plat_dyn: None,
            smp: None,
            debug: false,
        }
    }

    fn resolve_test_qemu_config(package: &str, target: &str) -> anyhow::Result<PathBuf> {
        let manifest_path = build::resolve_package_manifest_path(package, None)?;
        let app_dir = manifest_path
            .parent()
            .context("package manifest path has no parent directory")?;
        let qemu_config = app_dir.join(format!("qemu-{}.toml", arch_from_target(target)));
        if qemu_config.exists() {
            Ok(qemu_config)
        } else {
            bail!(
                "missing qemu config for package `{package}` and target `{target}` at {}",
                qemu_config.display()
            )
        }
    }

    async fn load_qemu_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::qemu::QemuConfig>> {
        match request.qemu_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
                .read_qemu_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
                .read_uboot_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let qemu = self.load_qemu_config(&request, &cargo).await?;
        self.app.qemu(cargo, request.build_info_path, qemu).await
    }

    async fn run_build_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app.uboot(cargo, request.build_info_path, uboot).await
    }
}

fn planned_qemu_test_flows(args: &ArgsTestQemu) -> &'static [QemuTestFlow] {
    if args.only_c {
        &[QemuTestFlow::C]
    } else if args.only_rust || !args.package.is_empty() {
        &[QemuTestFlow::Rust]
    } else {
        &[QemuTestFlow::Rust, QemuTestFlow::C]
    }
}

fn select_arceos_test_packages(requested: &[String]) -> anyhow::Result<Vec<&'static str>> {
    if requested.is_empty() {
        return Ok(test_qemu::ARCEOS_TEST_PACKAGES.to_vec());
    }

    let mut selected = Vec::with_capacity(requested.len());
    let mut seen = HashSet::new();

    for package in requested {
        let resolved = test_qemu::ARCEOS_TEST_PACKAGES
            .iter()
            .copied()
            .find(|candidate| *candidate == package)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported arceos rust test package `{}`. Supported packages are: {}",
                    package,
                    test_qemu::ARCEOS_TEST_PACKAGES.join(", ")
                )
            })?;
        if seen.insert(resolved) {
            selected.push(resolved);
        }
    }

    Ok(selected)
}

fn run_c_qemu_tests_with_hooks<PrepareConfig, RunTest>(
    workspace_root: &Path,
    target: &str,
    mut prepare_config: PrepareConfig,
    mut run_test: RunTest,
) -> anyhow::Result<()>
where
    PrepareConfig: FnMut(&Path) -> anyhow::Result<PathBuf>,
    RunTest: FnMut(&Path, &Path, &str, &[String], &CTestInvocation) -> anyhow::Result<()>,
{
    let arch = arch_from_target(target);
    let arceos_dir = workspace_root.join("os/arceos");
    let c_test_root = workspace_root.join("test-suit/arceos/c");

    if !arceos_dir.join("Makefile").exists() {
        bail!(
            "arceos Makefile not found at {}, required for C test builds",
            arceos_dir.display()
        );
    }

    let c_tests = discover_c_tests(&c_test_root)?;
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

        let mut test_failed = false;
        for invocation in &c_test.invocations {
            let result = run_test(&arceos_dir, &app_path, arch, &c_test.features, invocation)
                .with_context(|| {
                    format!(
                        "c test `{}` failed for `{}`",
                        c_test.name,
                        c_test_invocation_label(invocation)
                    )
                });
            if let Err(err) = result {
                eprintln!("failed: c/{}: {:#}", c_test.name, err);
                failed.push(c_test.name.clone());
                test_failed = true;
                break;
            }
        }

        if !test_failed {
            println!("ok: c/{}", c_test.name);
        }
    }

    test_qemu::finalize_qemu_test_run("arceos c", &failed)
}

fn run_single_c_qemu_test(
    arceos_dir: &Path,
    app_path: &Path,
    arch: &str,
    base_features: &[String],
    invocation: &CTestInvocation,
) -> anyhow::Result<()> {
    let make_args = build_c_test_make_args(app_path, arch, base_features, invocation);

    StdCommand::new("make")
        .current_dir(arceos_dir)
        .args(&make_args)
        .arg("defconfig")
        .exec()?;

    StdCommand::new("make")
        .current_dir(arceos_dir)
        .args(&make_args)
        .arg("build")
        .exec()?;

    let output = StdCommand::new("make")
        .current_dir(arceos_dir)
        .args(&make_args)
        .arg("justrun")
        .exec_capture()?;

    if let Some(expect_output) = &invocation.expect_output {
        verify_c_test_runtime_output(&output, &app_path.join(expect_output))?;
    }

    Ok(())
}

impl Default for ArceOS {
    fn default() -> Self {
        Self::new().expect("failed to initialize ArceOS")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::process::ExitStatusExt,
        sync::{Arc, Mutex},
    };

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
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
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
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
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
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
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
    fn command_parses_test_qemu_package_filter() {
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
            "riscv64gc-unknown-none-elf",
            "--package",
            "arceos-ipi",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("riscv64gc-unknown-none-elf"));
                    assert_eq!(args.package, vec!["arceos-ipi".to_string()]);
                    assert!(!args.only_rust);
                    assert!(!args.only_c);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
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
        std::fs::write(
            dir.join("helloworld/test_cmd"),
            "test_one \"LOG=info\" \"expect_info.out\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("empty")).unwrap();

        let tests = discover_c_tests(&dir).unwrap();
        // Only "helloworld" should be found, "empty" is not in C_TEST_NAMES
        assert!(tests.iter().any(|t| t.name == "helloworld"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_c_test_invocations_parses_test_cmd() {
        let dir = tempdir().unwrap();
        let test_cmd = dir.path().join("test_cmd");
        std::fs::write(
            &test_cmd,
            "test_one \"SMP=4 LOG=info FEATURES=sched-rr\" \"expect.out\"\nrm -f $APP/*.o\n",
        )
        .unwrap();

        let invocations = load_c_test_invocations(&test_cmd).unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].make_vars,
            vec![
                ("SMP".to_string(), "4".to_string()),
                ("LOG".to_string(), "info".to_string()),
                ("FEATURES".to_string(), "sched-rr".to_string())
            ]
        );
        assert_eq!(
            invocations[0].expect_output,
            Some(PathBuf::from("expect.out"))
        );
    }

    #[test]
    fn translate_bre_to_regex_handles_bre_quantifiers_and_literals() {
        let translated =
            translate_bre_to_regex(r"task 15 actually sleep 5\.[0-9]\+ seconds (2) ...");
        let regex = Regex::new(&translated).unwrap();
        assert!(regex.is_match("task 15 actually sleep 5.009334 seconds (2) ..."));
    }

    #[test]
    fn verify_c_test_runtime_output_matches_expected_lines_in_order() {
        let dir = tempdir().unwrap();
        let expected = dir.path().join("expect.out");
        std::fs::write(
            &expected,
            "Hello, C app!\nvalue = [0-9]\\+\nShutting down...\n",
        )
        .unwrap();
        let output = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"noise\nHello, C app!\nvalue = 42\nShutting down...\n".to_vec(),
            stderr: Vec::new(),
        };

        verify_c_test_runtime_output(&output, &expected).unwrap();
    }

    #[test]
    fn planned_qemu_test_flows_default_runs_rust_then_c() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            arch: None,
            target: Some("x86_64-unknown-none".to_string()),
            package: Vec::new(),
            only_rust: false,
            only_c: false,
        });

        assert_eq!(flows, &[QemuTestFlow::Rust, QemuTestFlow::C]);
    }

    #[test]
    fn planned_qemu_test_flows_only_rust_skips_c() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            arch: None,
            target: Some("x86_64-unknown-none".to_string()),
            package: Vec::new(),
            only_rust: true,
            only_c: false,
        });

        assert_eq!(flows, &[QemuTestFlow::Rust]);
    }

    #[test]
    fn planned_qemu_test_flows_only_c_skips_rust() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            arch: None,
            target: Some("x86_64-unknown-none".to_string()),
            package: Vec::new(),
            only_rust: false,
            only_c: true,
        });

        assert_eq!(flows, &[QemuTestFlow::C]);
    }

    #[test]
    fn planned_qemu_test_flows_package_filter_runs_only_rust() {
        let flows = planned_qemu_test_flows(&ArgsTestQemu {
            arch: None,
            target: Some("x86_64-unknown-none".to_string()),
            package: vec!["arceos-ipi".to_string()],
            only_rust: false,
            only_c: false,
        });

        assert_eq!(flows, &[QemuTestFlow::Rust]);
    }

    #[test]
    fn select_arceos_test_packages_defaults_to_all_packages() {
        let selected = select_arceos_test_packages(&[]).unwrap();
        assert_eq!(selected, test_qemu::ARCEOS_TEST_PACKAGES);
    }

    #[test]
    fn select_arceos_test_packages_accepts_known_package() {
        let selected = select_arceos_test_packages(&["arceos-ipi".to_string()]).unwrap();
        assert_eq!(selected, vec!["arceos-ipi"]);
    }

    #[test]
    fn select_arceos_test_packages_rejects_unknown_package() {
        let err = select_arceos_test_packages(&["unknown-package".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported arceos rust test package `unknown-package`")
        );
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
            move |_arceos_dir, app_path, _arch, _features, _invocation| {
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
