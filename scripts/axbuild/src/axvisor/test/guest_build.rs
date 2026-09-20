//! Build source guests through the existing ArceOS build pipeline before the host.
use std::{fs, path::Path};

use anyhow::{Context, ensure};
use serde::Deserialize;

use crate::context::{AppContext, BuildCliArgs};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestBuilds {
    arceos_build_configs: Vec<std::path::PathBuf>,
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
    let builds: GuestBuilds = toml::from_str(&contents)
        .with_context(|| format!("invalid guest build list {}", path.display()))?;
    for path in builds.arceos_build_configs {
        let config = directory.join(path).canonicalize()?;
        ensure!(
            config.starts_with(app.workspace_root()),
            "guest config escapes workspace"
        );
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
    }
    Ok(())
}
