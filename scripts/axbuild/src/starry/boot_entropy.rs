//! Session-scoped boot entropy for secure board Wi-Fi startup.

use std::{env, ffi::OsStr, fs, path::Path};

use anyhow::{Context, ensure};
use ostool::board::config::BoardRunConfig;

#[derive(Debug)]
pub(super) struct PreparedBootEntropy {
    _directory: tempfile::TempDir,
}

pub(super) fn prepare_for_secure_wifi(
    board_config: &mut BoardRunConfig,
) -> anyhow::Result<Option<PreparedBootEntropy>> {
    let requested = secure_wifi_requested(
        env::var_os("STARRY_WIFI_SSID").as_deref(),
        env::var_os("STARRY_WIFI_PASSWORD").as_deref(),
    )?;
    prepare(board_config, requested)
}

fn secure_wifi_requested(ssid: Option<&OsStr>, password: Option<&OsStr>) -> anyhow::Result<bool> {
    match (ssid, password) {
        (None, None) => Ok(false),
        (Some(_), Some(_)) => Ok(true),
        _ => anyhow::bail!("both STARRY_WIFI_SSID and STARRY_WIFI_PASSWORD must be set"),
    }
}

fn prepare(
    board_config: &mut BoardRunConfig,
    requested: bool,
) -> anyhow::Result<Option<PreparedBootEntropy>> {
    if !requested {
        return Ok(None);
    }

    let source = board_config
        .dtb_file
        .as_deref()
        .map(Path::new)
        .context("secure Wi-Fi startup requires dtb_file")?;
    let source_bytes = fs::read(source)
        .with_context(|| format!("failed to read Wi-Fi boot DTB {}", source.display()))?;
    let mut fdt = fdt_edit::Fdt::from_bytes(&source_bytes)
        .with_context(|| format!("failed to parse Wi-Fi boot DTB {}", source.display()))?;
    let chosen = fdt
        .get_by_path("/chosen")
        .map(|node| node.id())
        .context("secure Wi-Fi boot DTB does not contain /chosen")?;

    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).context("host OS random source is unavailable")?;
    fdt.node_mut(chosen)
        .expect("chosen node id belongs to this FDT")
        .set_property(fdt_edit::Property::new("rng-seed", seed.to_vec()));

    let directory = tempfile::Builder::new()
        .prefix("starry-wifi-boot-")
        .tempdir()
        .context("failed to create temporary Wi-Fi boot directory")?;
    let destination = directory.path().join("boot-entropy.dtb");
    fs::write(&destination, fdt.encode().as_ref())
        .with_context(|| format!("failed to write {}", destination.display()))?;
    ensure!(
        destination.is_file(),
        "temporary Wi-Fi boot DTB was not created"
    );
    board_config.dtb_file = Some(destination.to_string_lossy().into_owned());

    Ok(Some(PreparedBootEntropy {
        _directory: directory,
    }))
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, path::Path};

    use fdt_edit::{Fdt, Node};
    use ostool::board::config::BoardRunConfig;

    use super::{prepare, secure_wifi_requested};

    #[test]
    fn secure_wifi_environment_requires_a_complete_pair() {
        assert!(!secure_wifi_requested(None, None).unwrap());
        assert!(
            secure_wifi_requested(Some(OsStr::new("ssid")), Some(OsStr::new("password"))).unwrap()
        );
        assert!(secure_wifi_requested(Some(OsStr::new("ssid")), None).is_err());
        assert!(secure_wifi_requested(None, Some(OsStr::new("password"))).is_err());
    }

    #[test]
    fn secure_wifi_uses_a_fresh_temporary_dtb_without_changing_the_source() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.dtb");
        let mut source_fdt = Fdt::new();
        let root = source_fdt.root_id();
        source_fdt.add_node(root, Node::new("chosen"));
        let source_bytes = source_fdt.encode().as_ref().to_vec();
        fs::write(&source_path, &source_bytes).unwrap();

        let mut board_config = BoardRunConfig {
            dtb_file: Some(source_path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let prepared = prepare(&mut board_config, true).unwrap().unwrap();
        let temporary_path = board_config.dtb_file.as_ref().unwrap().clone();

        assert_ne!(temporary_path, source_path.to_string_lossy());
        assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
        let temporary_bytes = fs::read(&temporary_path).unwrap();
        let temporary_fdt = Fdt::from_bytes(&temporary_bytes).unwrap();
        let seed = temporary_fdt
            .get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property("rng-seed")
            .unwrap();
        assert_eq!(seed.data.len(), 32);
        assert_ne!(seed.data, [0; 32]);

        drop(prepared);
        assert!(!Path::new(&temporary_path).exists());
    }
}
