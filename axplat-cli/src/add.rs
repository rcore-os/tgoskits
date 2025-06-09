use clap::{Args, Parser};

#[derive(Args, Debug)]
#[group(required = true, multiple = true)]
struct PlatformCrate {
    /// Reference to a platform package to add as a dependency
    #[arg(long_help = "\
Reference to a platform package to add as a dependency

You can reference a package by:
- `<name>`, like `axplat-cli add axplat-x86-pc` (latest version will be used)
- `<name>@<version-req>`, like `axplat-cli add axplat-x86-pc@0.1` or `axplat-cli add axplat-x86-pc@=0.1.2`")]
    dep_id: Vec<String>,

    /// Filesystem path to local crate to add
    #[arg(long, help_heading = "Source")]
    path: Option<String>,

    /// Git repository location
    ///
    /// Without any other information, cargo will use latest commit on the main branch.
    #[arg(long, value_name = "URI", help_heading = "Source")]
    git: Option<String>,
}

/// Add platform package dependencies to a Cargo.toml manifest file
#[derive(Parser, Debug)]
#[command(long_about = "Add a platform package as a dependency")]
pub struct CommandAdd {
    /// Reference to a platform package to add as a dependency
    #[command(flatten)]
    plat: PlatformCrate,

    /// Package to modify
    #[arg(
        short = 'p',
        long,
        value_name = "SPEC",
        help_heading = "Package Selection"
    )]
    package: Option<String>,

    /// Path to Cargo.toml
    #[arg(long, value_name = "PATH", help_heading = "Package Selection")]
    manifest_path: Option<String>,

    /// Git branch to download the crate from
    #[arg(long, help_heading = "Source")]
    branch: Option<String>,

    /// Git tag to download the crate from
    #[arg(long, help_heading = "Source")]
    tag: Option<String>,

    /// Git reference to download the crate from
    ///
    /// This is the catch all, handling hashes to named references in remote repositories.
    #[arg(long, help_heading = "Source")]
    rev: Option<String>,

    /// Space or comma separated list of features to activate
    #[arg(short = 'F', long)]
    features: Option<String>,

    /// Add as dependency to the given target platform
    #[arg(long)]
    target: Option<String>,
}

pub fn add_platform(args: CommandAdd) {
    crate::run_cargo_command("add", |cmd| {
        args.plat.dep_id.iter().for_each(|dep| {
            cmd.arg(dep);
        });
        if let Some(git) = &args.plat.git {
            cmd.arg("--git").arg(git);
        }
        if let Some(path) = &args.plat.path {
            cmd.arg("--path").arg(path);
        }
        if let Some(package) = &args.package {
            cmd.arg("-p").arg(package);
        }
        if let Some(manifest_path) = &args.manifest_path {
            cmd.arg("--manifest-path").arg(manifest_path);
        }
        if let Some(package) = &args.branch {
            cmd.arg("--branch").arg(package);
        }
        if let Some(package) = &args.tag {
            cmd.arg("--tag").arg(package);
        }
        if let Some(package) = &args.rev {
            cmd.arg("--rev").arg(package);
        }
        if let Some(features) = &args.features {
            cmd.arg("-F").arg(features);
        }
        if let Some(target) = &args.target {
            cmd.arg("--target").arg(target);
        }
    });
}
