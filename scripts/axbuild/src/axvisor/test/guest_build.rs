//! Build source guests through the existing ArceOS and Starry build pipelines before the host.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::Deserialize;

use crate::context::{AppContext, BuildCliArgs, StarryCliArgs};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestBuilds {
    #[serde(default)]
    arceos_build_configs: Vec<PathBuf>,
    #[serde(default)]
    starry_build_configs: Vec<PathBuf>,
}

fn parse_guest_builds(contents: &str) -> anyhow::Result<GuestBuilds> {
    Ok(toml::from_str(contents)?)
}

pub(super) async fn prepare(app: &mut AppContext, board_config: &Path) -> anyhow::Result<()> {
    let directory = board_config
        .parent()
        .context("board config has no directory")?;
    let path = directory.join("guest-builds.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let builds = parse_guest_builds(&contents)
        .with_context(|| format!("invalid guest build list {}", path.display()))?;
    for path in builds.arceos_build_configs {
        prepare_arceos_guest(app, directory, path).await?;
    }
    for path in builds.starry_build_configs {
        prepare_starry_guest(app, directory, path).await?;
    }
    Ok(())
}

async fn prepare_arceos_guest(
    app: &mut AppContext,
    directory: &Path,
    path: PathBuf,
) -> anyhow::Result<()> {
    let config = resolve_guest_config(app, directory, path)?;
    let guest = crate::arceos::build::load_arceos_build_file(&config)?;
    ensure!(
        guest.package.is_some() && guest.target.is_some(),
        "test guest config must declare package and target"
    );
    let (request, _) = app.prepare_arceos_request(
        BuildCliArgs {
            config: Some(config),
            ..Default::default()
        },
        None,
        None,
        crate::arceos::build::resolve_build_info_path,
    )?;
    let cargo = crate::arceos::build::load_cargo_config(&request, app.workspace_context())?;
    ensure!(cargo.to_bin, "board guest build must enable to_bin");
    println!("prepare board guest: {}", request.package);
    app.build(cargo, request.build_info_path).await?;
    Ok(())
}

async fn prepare_starry_guest(
    app: &mut AppContext,
    directory: &Path,
    path: PathBuf,
) -> anyhow::Result<()> {
    let config = resolve_guest_config(app, directory, path)?;
    // Resolve the target from the guest config itself so the build never depends
    // on a previously stored `.starry.toml` snapshot.
    let target = crate::starry::build::load_target_from_build_config(&config)?
        .context("Starry test guest config must declare `target`")?;
    let (request, _) = app.prepare_starry_request(
        StarryCliArgs {
            config: Some(config),
            target: Some(target),
            ..Default::default()
        },
        None,
        None,
        crate::starry::build::resolve_build_info_path,
    )?;
    let cargo = crate::starry::build::load_cargo_config(&request, app.workspace_context())?;
    ensure!(cargo.to_bin, "board guest build must enable to_bin");
    println!(
        "prepare board Starry guest: {} ({})",
        request.package, request.target
    );
    // Reuse the full `cargo xtask starry build` entry so the AArch64
    // future-incompat report session, image build, kallsyms/bin post-processing
    // and error propagation stay identical to the standalone Starry build.
    // This also refreshes `starryos.bin` from the freshly linked ELF so board
    // configs that load the artifact from `${workspace}/target` see the new
    // image.
    crate::starry::build::build_starry_artifact(app, &request, cargo).await?;
    Ok(())
}

fn resolve_guest_config(
    app: &AppContext,
    directory: &Path,
    path: PathBuf,
) -> anyhow::Result<PathBuf> {
    let config = directory.join(path).canonicalize()?;
    ensure!(
        config.starts_with(app.workspace_root()),
        "guest config {} escapes workspace",
        config.display()
    );
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arceos_only_guest_builds() {
        let builds = parse_guest_builds(r#"arceos_build_configs = ["guest-build.toml"]"#).unwrap();
        assert_eq!(
            builds.arceos_build_configs,
            vec![PathBuf::from("guest-build.toml")]
        );
        assert!(builds.starry_build_configs.is_empty());
    }

    #[test]
    fn parses_starry_only_guest_builds() {
        let builds =
            parse_guest_builds(r#"starry_build_configs = ["starry-guest-build.toml"]"#).unwrap();
        assert!(builds.arceos_build_configs.is_empty());
        assert_eq!(
            builds.starry_build_configs,
            vec![PathBuf::from("starry-guest-build.toml")]
        );
    }

    #[test]
    fn parses_combined_guest_builds() {
        let builds = parse_guest_builds(
            r#"
arceos_build_configs = ["guest-build.toml"]
starry_build_configs = ["../starry-axivc-benchmark-build.toml"]
"#,
        )
        .unwrap();
        assert_eq!(
            builds.arceos_build_configs,
            vec![PathBuf::from("guest-build.toml")]
        );
        assert_eq!(
            builds.starry_build_configs,
            vec![PathBuf::from("../starry-axivc-benchmark-build.toml")]
        );
    }

    #[test]
    fn rejects_unknown_guest_build_fields() {
        assert!(parse_guest_builds(r#"linux_build_configs = ["guest-build.toml"]"#).is_err());
    }
}
