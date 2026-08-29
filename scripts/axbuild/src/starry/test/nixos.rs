use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::OnceLock,
};

use anyhow::{Context, bail};
use regex::Regex;

use super::ArgsTestNixos;
use crate::{
    context::{SnapshotPersistence, StarryCliArgs},
    starry::{Starry, build},
};

const ARCH: &str = "x86_64";
const TARGET: &str = "x86_64-unknown-none";
const BUILD_CONFIG: &str = "apps/starry/nixos/build-x86_64-unknown-none.toml";
const APP_FLAKE_DIR: &str = "apps/starry/nixos";
const TEST_FLAKE_DIR: &str = "nixos-tests/starryos";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NixosCase {
    pub(crate) name: &'static str,
    pub(crate) arch: &'static str,
    pub(crate) target: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NixosAction {
    List,
    Run {
        build_config: PathBuf,
        case_name: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputMode {
    CaptureStdout,
    Inherit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessSpec {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) output: OutputMode,
}

pub(super) async fn run(starry: &mut Starry, args: ArgsTestNixos) -> anyhow::Result<()> {
    let workspace = starry.app.workspace_root().to_path_buf();
    match plan_nixos_action(&workspace, &args)? {
        NixosAction::List => {
            print_cases();
            Ok(())
        }
        NixosAction::Run {
            build_config,
            case_name,
        } => {
            let kernel = build_kernel(starry, build_config).await?;
            let kernel = validate_kernel(&kernel)?;
            let kernel_nar_hash = hash_kernel(&workspace, &kernel)?;
            println!("[axbuild] Starry nixosTest kernel={}", kernel.display());
            println!("[axbuild] Starry nixosTest kernel_nar_hash={kernel_nar_hash}");

            let command = nix_test_command(&workspace, &kernel, &kernel_nar_hash, &case_name);
            run_inherited(&workspace, &command)
        }
    }
}

pub(crate) fn supported_cases() -> &'static [NixosCase] {
    const CASES: &[NixosCase] = &[
        NixosCase {
            name: "boot",
            arch: ARCH,
            target: TARGET,
        },
        NixosCase {
            name: "service",
            arch: ARCH,
            target: TARGET,
        },
        NixosCase {
            name: "service-fail",
            arch: ARCH,
            target: TARGET,
        },
        NixosCase {
            name: "unsupported",
            arch: ARCH,
            target: TARGET,
        },
    ];
    CASES
}

pub(crate) fn plan_nixos_action(
    workspace: &Path,
    args: &ArgsTestNixos,
) -> anyhow::Result<NixosAction> {
    if args.list {
        return Ok(NixosAction::List);
    }

    let arch = args
        .arch
        .as_deref()
        .context("Starry nixosTest requires `--arch x86_64`")?;
    let test_case = args
        .test_case
        .as_deref()
        .context("Starry nixosTest requires `--test-case`")?;
    if arch != ARCH {
        bail!("unsupported Starry nixosTest architecture `{arch}`; supported: {ARCH}");
    }
    if !supported_cases().iter().any(|case| case.name == test_case) {
        let supported = supported_cases()
            .iter()
            .map(|case| case.name)
            .collect::<Vec<_>>()
            .join(", ");
        bail!("unsupported Starry nixosTest case `{test_case}`; supported: {supported}");
    }

    Ok(NixosAction::Run {
        build_config: workspace.join(BUILD_CONFIG),
        case_name: test_case.to_string(),
    })
}

pub(crate) fn validate_kernel(path: &Path) -> anyhow::Result<PathBuf> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "prepared Starry nixosTest kernel is missing: {}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        bail!(
            "prepared Starry nixosTest kernel is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() == 0 {
        bail!(
            "prepared Starry nixosTest kernel is empty: {}",
            path.display()
        );
    }
    path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize Starry nixosTest kernel {}",
            path.display()
        )
    })
}

pub(crate) fn validate_nar_hash(hash: &str) -> anyhow::Result<String> {
    static SHA256_SRI: OnceLock<Regex> = OnceLock::new();
    let pattern = SHA256_SRI
        .get_or_init(|| Regex::new(r"^sha256-[A-Za-z0-9+/]{43}=$").expect("valid SRI regex"));
    if !pattern.is_match(hash) {
        bail!("`nix hash path` returned an invalid SHA-256 SRI NAR hash: {hash:?}");
    }
    Ok(hash.to_string())
}

pub(crate) fn nix_hash_command(kernel: &Path) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from("nix"),
        args: vec![
            "hash".to_string(),
            "path".to_string(),
            "--type".to_string(),
            "sha256".to_string(),
            "--sri".to_string(),
            kernel.display().to_string(),
        ],
        output: OutputMode::CaptureStdout,
    }
}

pub(crate) fn nix_test_command(
    workspace: &Path,
    kernel: &Path,
    kernel_nar_hash: &str,
    case_name: &str,
) -> ProcessSpec {
    let app_flake = format!("path:{}/{}", workspace.display(), APP_FLAKE_DIR);
    let test_flake = format!("path:{}/{}", workspace.display(), TEST_FLAKE_DIR);
    let expression = format!(
        "let testFlake = builtins.getFlake {}; appFlake = builtins.getFlake {}; system = {}; test \
         = testFlake.lib.${{system}}.mkStarryNixosTest {{ kernelPath = {}; kernelNarHash = {}; \
         starryNixos = appFlake.lib.${{system}}.starryNixos; caseName = {}; }}; in assert \
         testFlake.inputs.nixpkgs.outPath == appFlake.inputs.nixpkgs.outPath; builtins.trace \
         (\"[axbuild] Starry nixosTest kernel_store=\" + builtins.toString test.kernelStorePath) \
         (builtins.trace (\"[axbuild] Starry nixosTest system_toplevel=\" + builtins.toString \
         test.systemToplevel) test)",
        nix_string(&test_flake),
        nix_string(&app_flake),
        nix_string("x86_64-linux"),
        nix_string(&kernel.display().to_string()),
        nix_string(kernel_nar_hash),
        nix_string(case_name),
    );
    ProcessSpec {
        program: PathBuf::from("nix"),
        args: vec![
            "build".to_string(),
            "--impure".to_string(),
            "--print-build-logs".to_string(),
            "--no-link".to_string(),
            "--expr".to_string(),
            expression,
        ],
        output: OutputMode::Inherit,
    }
}

pub(crate) fn ensure_success(status: ExitStatus, operation: &str) -> anyhow::Result<()> {
    if status.success() {
        Ok(())
    } else if let Some(code) = status.code() {
        bail!("{operation} exited with status {code}")
    } else {
        bail!("{operation} terminated without an exit status")
    }
}

pub(crate) fn configure_p1_build_info(
    mut build_info: build::StarryBuildInfo,
) -> build::StarryBuildInfo {
    // The P1 contract observes systemd's serial markers, while Info-level
    // per-task kernel logging can consume the bounded TCG terminal window.
    build_info.log = build::LogLevel::Warn;
    build_info
}

fn print_cases() {
    for case in supported_cases() {
        println!("{}\tarch={}\ttarget={}", case.name, case.arch, case.target);
    }
}

async fn build_kernel(starry: &mut Starry, build_config: PathBuf) -> anyhow::Result<PathBuf> {
    let mut request = starry.prepare_request(
        StarryCliArgs {
            config: Some(build_config),
            arch: Some(ARCH.to_string()),
            target: Some(TARGET.to_string()),
            smp: None,
            debug: false,
        },
        None,
        None,
        SnapshotPersistence::Discard,
    )?;
    starry.app.set_debug_mode(false)?;
    let build_info = build::load_build_info(&request)
        .context("failed to load the Starry nixosTest build configuration")?;
    request.build_info_override = Some(configure_p1_build_info(build_info));
    let cargo = build::load_cargo_config(&request)
        .context("failed to prepare the Starry nixosTest build configuration")?;
    let output = starry
        .build_artifact(&request, cargo)
        .await
        .context("failed to build the Starry nixosTest kernel")?;
    let elf = output.elf_path().to_path_buf();
    starry
        .app
        .prepare_elf_artifact(elf.clone(), true)
        .await
        .context("failed to prepare the Starry x86_64 UEFI image")?;
    Ok(elf.with_extension("bin"))
}

fn hash_kernel(workspace: &Path, kernel: &Path) -> anyhow::Result<String> {
    let command = nix_hash_command(kernel);
    let output = Command::new(&command.program)
        .args(&command.args)
        .current_dir(workspace)
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to run `{}`", render_command(&command)))?;
    ensure_success(output.status, "nix hash path")?;
    let stdout =
        String::from_utf8(output.stdout).context("`nix hash path` returned non-UTF-8 output")?;
    validate_nar_hash(stdout.trim())
}

fn run_inherited(workspace: &Path, command: &ProcessSpec) -> anyhow::Result<()> {
    debug_assert_eq!(command.output, OutputMode::Inherit);
    eprintln!("[axbuild] {}", render_command(command));
    let status = Command::new(&command.program)
        .args(&command.args)
        .current_dir(workspace)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to run `{}`", render_command(command)))?;
    ensure_success(status, "Starry nixosTest")
}

fn nix_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

fn render_command(command: &ProcessSpec) -> String {
    std::iter::once(command.program.display().to_string())
        .chain(command.args.iter().map(|arg| format!("{arg:?}")))
        .collect::<Vec<_>>()
        .join(" ")
}
