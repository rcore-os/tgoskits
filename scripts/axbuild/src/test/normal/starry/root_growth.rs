//! Fresh raw ext4 growth fixtures, isolated from shared and cached root images.

use std::{fs, path::Path};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::execution::case::TestQemuCase;

/// Validated size change applied only to the disposable, post-injection copy.
pub(super) struct RootGrowthFixture {
    /// Source image length before any test-specific change.
    original_bytes: u64,
    /// Required raw filesystem capacity observed through the guest ABI.
    target_bytes: u64,
}

#[derive(Deserialize)]
struct GrowthConfig {
    /// Extra sparse bytes appended to each fresh root image, never its cache.
    rootfs_growth_bytes: Option<u64>,
}

impl RootGrowthFixture {
    /// Installs the capacity assertion before the grouped runner is generated.
    pub(super) fn prepare(case: &mut TestQemuCase, source: &Path) -> anyhow::Result<Option<Self>> {
        let config: GrowthConfig = toml::from_str(&fs::read_to_string(&case.qemu_config_path)?)?;
        let Some(extra_bytes) = config.rootfs_growth_bytes else {
            return Ok(None);
        };
        if extra_bytes == 0 || !extra_bytes.is_multiple_of(4096) || !case.is_grouped() {
            bail!(
                "rootfs_growth_bytes requires a positive 4096-byte multiple and grouped commands"
            );
        }
        let original_bytes = fs::metadata(source)?.len();
        let target_bytes = original_bytes
            .checked_add(extra_bytes)
            .context("root growth fixture size overflow")?;
        for command in &mut case.test_commands {
            *command = format!("export STARRY_ROOT_EXPECTED_BYTES={target_bytes}; {command}");
        }
        Ok(Some(Self {
            original_bytes,
            target_bytes,
        }))
    }

    /// Enlarges the owned copy after cache preparation, leaving ext4 unchanged.
    pub(super) fn extend(&self, rootfs_path: Option<&Path>) -> anyhow::Result<()> {
        let rootfs_path =
            rootfs_path.context("root growth fixture requires an owned disposable rootfs copy")?;
        let image = fs::OpenOptions::new().write(true).open(rootfs_path)?;
        if image.metadata()?.len() != self.original_bytes {
            bail!("root growth fixture image length changed during asset preparation");
        }
        image.set_len(self.target_bytes)?;
        println!(
            "root growth fixture: original_bytes={} target_bytes={}",
            self.original_bytes, self.target_bytes
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_only_resizes_the_disposable_copy_and_preserves_existing_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("shared.img");
        let copy = directory.path().join("run.img");
        let original = vec![0x5a; 8192];
        fs::write(&source, &original).unwrap();
        fs::copy(&source, &copy).unwrap();
        let fixture = RootGrowthFixture {
            original_bytes: 8192,
            target_bytes: 12288,
        };
        assert!(fixture.extend(None).is_err());
        assert_eq!(fs::read(&source).unwrap(), original);
        fixture.extend(Some(&copy)).unwrap();
        let grown = fs::read(&copy).unwrap();
        assert_eq!(grown.len(), 12288);
        assert_eq!(&grown[..original.len()], original.as_slice());
        assert!(grown[original.len()..].iter().all(|byte| *byte == 0));
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(fixture.extend(Some(&copy)).is_err());
    }
}
