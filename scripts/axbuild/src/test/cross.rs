use std::{ffi::OsString, path::Path, process::Command};

use anyhow::Context;
use clap::Args;

use crate::{
    context::cross_compile_spec_for_arch_checked,
    support::process::{ProcessExt, find_host_binary_candidates},
};

const STATIC_RUSTFLAGS: &str = "-C target-feature=+crt-static";

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub(crate) struct CrossTestArgs {
    /// Target architecture used to select the Rust musl target and qemu-user runner
    #[arg(long)]
    pub(crate) arch: String,

    /// Workspace package to test; may be repeated
    #[arg(short = 'p', long = "package", required = true)]
    pub(crate) packages: Vec<String>,

    /// Cargo features, separated by commas
    #[arg(long, value_delimiter = ',')]
    pub(crate) features: Vec<String>,

    /// Disable package default features
    #[arg(long)]
    pub(crate) no_default_features: bool,

    /// Test only the package library
    #[arg(long)]
    pub(crate) lib: bool,

    /// Optional cargo test name filter
    pub(crate) name_filter: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CrossTestPlan {
    cargo_args: Vec<String>,
    envs: Vec<(String, OsString)>,
}

impl CrossTestPlan {
    fn new(args: &CrossTestArgs, rust_target: &str, linker: &Path, qemu_runner: &Path) -> Self {
        let mut cargo_args = vec!["test".to_string()];
        for package in &args.packages {
            cargo_args.extend(["--package".to_string(), package.clone()]);
        }
        cargo_args.extend(["--target".to_string(), rust_target.to_string()]);
        if !args.features.is_empty() {
            cargo_args.extend(["--features".to_string(), args.features.join(",")]);
        }
        if args.no_default_features {
            cargo_args.push("--no-default-features".to_string());
        }
        if args.lib {
            cargo_args.push("--lib".to_string());
        }
        if let Some(name_filter) = &args.name_filter {
            cargo_args.push(name_filter.clone());
        }

        let target_env = rust_target.to_uppercase().replace('-', "_");
        let envs = vec![
            (
                format!("CARGO_TARGET_{target_env}_LINKER"),
                linker.as_os_str().to_os_string(),
            ),
            (
                format!("CARGO_TARGET_{target_env}_RUNNER"),
                qemu_runner.as_os_str().to_os_string(),
            ),
            ("RUSTFLAGS".to_string(), OsString::from(STATIC_RUSTFLAGS)),
        ];

        Self { cargo_args, envs }
    }
}

pub(crate) fn run(args: CrossTestArgs) -> anyhow::Result<()> {
    let spec = cross_compile_spec_for_arch_checked(&args.arch)?;
    let qemu_runner = find_host_binary_candidates(spec.qemu_user_binaries)?;
    let linker = find_host_binary_candidates(&["rust-lld"])?;

    install_rust_target(spec.rust_musl_target)?;
    let plan = CrossTestPlan::new(&args, spec.rust_musl_target, &linker, &qemu_runner);

    let mut command = Command::new("cargo");
    command.args(&plan.cargo_args).envs(
        plan.envs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_os_str())),
    );
    command.exec().with_context(|| {
        format!(
            "failed to run workspace crate tests for `{}` through {}",
            args.arch,
            qemu_runner.display()
        )
    })
}

fn install_rust_target(target: &str) -> anyhow::Result<()> {
    let mut command = Command::new("rustup");
    command.args(["target", "add", target]);
    command
        .exec()
        .with_context(|| format!("failed to install Rust target `{target}` via rustup"))
}
