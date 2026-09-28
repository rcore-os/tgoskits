use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use object::{Object as _, ObjectSection as _};

use super::ArgsTestQemu;
use crate::{
    context::{SnapshotPersistence, StarryCliArgs, axbuild_tmp_dir},
    rootfs::qemu::{RootfsPatchMode, RootfsPatchOptions, RootfsWritePolicy},
    starry::{Starry, rootfs},
    test::qemu as qemu_test,
};

struct ExternalRun {
    build_config: PathBuf,
    qemu_config: PathBuf,
    rootfs: PathBuf,
    fixed_elf: Option<PathBuf>,
}

impl Starry {
    pub(super) async fn run_external_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        let external = ExternalRun::from_args(args)?;
        let request = self.prepare_request(
            StarryCliArgs {
                config: Some(external.build_config.clone()),
                arch: None,
                target: None,
                smp: None,
                debug: false,
            },
            Some(external.qemu_config.clone()),
            None,
            SnapshotPersistence::Discard,
        )?;
        self.app.set_debug_mode(request.debug)?;

        let fixed_elf = external
            .fixed_elf
            .as_deref()
            .map(|path| FixedElf::open(path, self.app.target_dir()))
            .transpose()?;
        let (request, cargo) = Self::qemu_group_build_context(
            &request,
            &external.build_config,
            self.app.workspace_context(),
        )?;
        let output = self
            .build_artifact(&request, cargo.clone())
            .await
            .context("failed to build the external Starry QEMU artifact")?;
        let built_elf = output.elf_path().to_path_buf();

        if let Some(fixed) = &fixed_elf {
            fixed.validate_against(&built_elf)?;
            fixed.ensure_unchanged()?;
        }

        let mut qemu = self
            .app
            .read_qemu_config_from_path_for_cargo(&cargo, &external.qemu_config)
            .await
            .with_context(|| {
                format!(
                    "failed to read external QEMU config {}",
                    external.qemu_config.display()
                )
            })?;
        rootfs::patch_rootfs(
            &mut qemu,
            &external.rootfs,
            RootfsPatchOptions {
                mode: RootfsPatchMode::EnsureDiskBootNet,
                write_policy: RootfsWritePolicy::Discard,
            },
        )?;
        qemu_test::apply_timeout_scale(&mut qemu);

        let staging = if let Some(fixed) = &fixed_elf {
            Some(fixed.activate(&mut self.app, qemu.to_bin).await?)
        } else {
            self.app
                .prepare_elf_artifact(built_elf, qemu.to_bin)
                .await?;
            None
        };

        println!("running one external Starry QEMU case");
        println!("  build config: {}", external.build_config.display());
        println!("  qemu config: {}", external.qemu_config.display());
        println!("  rootfs: {} (writes discarded)", external.rootfs.display());
        if let Some(fixed) = &fixed_elf {
            println!("  fixed elf: {}", fixed.path.display());
        }

        let run_result = self
            .app
            .run_qemu_with_axtest_coverage(&cargo, qemu, None)
            .await;
        drop(staging);

        let immutable_result = fixed_elf
            .as_ref()
            .map(FixedElf::ensure_unchanged)
            .transpose()
            .map(|_| ());
        combine_run_and_integrity_results(run_result, immutable_result)
    }
}

impl ExternalRun {
    fn from_args(args: ArgsTestQemu) -> anyhow::Result<Self> {
        if args.test_case.is_some() || args.list || args.arch.is_some() || args.target.is_some() {
            bail!(
                "external Starry QEMU inputs cannot be combined with architecture, target, case, \
                 or list selection"
            );
        }
        let (Some(build_config), Some(qemu_config), Some(rootfs)) =
            (args.build_config, args.qemu_config, args.rootfs)
        else {
            bail!("external Starry QEMU runs require --build-config, --qemu-config, and --rootfs");
        };
        Ok(Self {
            build_config: canonical_regular_file(&build_config, "build config")?,
            qemu_config: canonical_regular_file(&qemu_config, "QEMU config")?,
            rootfs: canonical_regular_file(&rootfs, "rootfs image")?,
            fixed_elf: args.fixed_elf,
        })
    }
}

fn canonical_regular_file(path: &Path, kind: &str) -> anyhow::Result<PathBuf> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve {kind} {}", path.display()))?;
    if !path
        .metadata()
        .with_context(|| format!("failed to inspect {kind} {}", path.display()))?
        .is_file()
    {
        bail!("{kind} is not a regular file: {}", path.display());
    }
    Ok(path)
}

fn combine_run_and_integrity_results(
    run_result: anyhow::Result<()>,
    integrity_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (run_result, integrity_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(run_error), Err(integrity_error)) => Err(anyhow::anyhow!(
            "{run_error:#}; fixed ELF integrity check also failed: {integrity_error:#}"
        )),
    }
}

struct FixedElf {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl FixedElf {
    fn open(path: &Path, target_dir: &Path) -> anyhow::Result<Self> {
        if !path.is_absolute() {
            bail!("fixed Starry ELF path must be absolute: {}", path.display());
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect fixed ELF {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "fixed Starry ELF must be a regular non-symlink file: {}",
                path.display()
            );
        }
        let path = fs::canonicalize(path)
            .with_context(|| format!("failed to resolve fixed ELF {}", path.display()))?;
        let target_dir = fs::canonicalize(target_dir).with_context(|| {
            format!(
                "failed to resolve Cargo target directory {}",
                target_dir.display()
            )
        })?;
        if path.starts_with(&target_dir) {
            bail!(
                "fixed Starry ELF must be outside the Cargo target directory: {}",
                path.display()
            );
        }
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read fixed ELF {}", path.display()))?;
        parse_elf(&bytes, &path)?;
        Ok(Self { path, bytes })
    }

    fn validate_against(&self, built_elf: &Path) -> anyhow::Result<()> {
        let built_bytes = fs::read(built_elf)
            .with_context(|| format!("failed to read built ELF {}", built_elf.display()))?;
        let built = parse_elf(&built_bytes, built_elf)?;
        let fixed = parse_elf(&self.bytes, &self.path)?;
        if built.architecture() != fixed.architecture() || built.entry() != fixed.entry() {
            bail!(
                "built and fixed Starry ELF architecture or entry point differs (built: {:?} \
                 entry={:#x}, fixed: {:?} entry={:#x})",
                built.architecture(),
                built.entry(),
                fixed.architecture(),
                fixed.entry()
            );
        }
        ensure_section_snapshots_match(
            &comparable_elf_sections(&built, built_elf)?,
            &comparable_elf_sections(&fixed, &self.path)?,
        )
    }

    fn ensure_unchanged(&self) -> anyhow::Result<()> {
        let current = fs::read(&self.path)
            .with_context(|| format!("failed to re-read fixed ELF {}", self.path.display()))?;
        if current != self.bytes {
            bail!(
                "fixed Starry ELF changed during the run: {}",
                self.path.display()
            );
        }
        Ok(())
    }

    async fn activate(
        &self,
        app: &mut crate::context::AppContext,
        to_bin: bool,
    ) -> anyhow::Result<tempfile::TempDir> {
        let staging_root = axbuild_tmp_dir(app.workspace_root()).join("external-elf");
        fs::create_dir_all(&staging_root)?;
        let staging = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(&staging_root)
            .context("failed to create fixed ELF staging directory")?;
        let staged_elf = staging.path().join("starryos");
        fs::write(&staged_elf, &self.bytes)
            .with_context(|| format!("failed to stage fixed ELF at {}", staged_elf.display()))?;
        app.prepare_elf_artifact(staged_elf, to_bin).await?;
        Ok(staging)
    }
}

fn parse_elf<'a>(bytes: &'a [u8], path: &Path) -> anyhow::Result<object::File<'a>> {
    object::File::parse(bytes).with_context(|| format!("failed to parse ELF {}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ComparableElfSection {
    name: String,
    address: u64,
    size: u64,
    data: Vec<u8>,
}

fn comparable_elf_sections(
    file: &object::File<'_>,
    path: &Path,
) -> anyhow::Result<Vec<ComparableElfSection>> {
    let mut sections = Vec::new();
    for section in file.sections() {
        let name = section
            .name()
            .with_context(|| format!("invalid section name in {}", path.display()))?;
        let coverage_metadata = matches!(name, "__llvm_covfun" | "__llvm_covmap");
        if name == ".kallsyms" || section.address() == 0 && !coverage_metadata {
            continue;
        }
        sections.push(ComparableElfSection {
            name: name.to_string(),
            address: section.address(),
            size: section.size(),
            data: section
                .data()
                .with_context(|| format!("failed to read section {name} in {}", path.display()))?
                .to_vec(),
        });
    }
    sections.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.address.cmp(&right.address))
    });
    Ok(sections)
}

fn ensure_section_snapshots_match(
    built: &[ComparableElfSection],
    fixed: &[ComparableElfSection],
) -> anyhow::Result<()> {
    if built != fixed {
        bail!("built Starry ELF executable or coverage sections differ from the fixed ELF");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_comparison_rejects_loaded_code_changes() {
        let base = ComparableElfSection {
            name: ".text".to_string(),
            address: 0x1000,
            size: 2,
            data: vec![1, 2],
        };
        assert!(
            ensure_section_snapshots_match(std::slice::from_ref(&base), &[base.clone()]).is_ok()
        );

        let changed = ComparableElfSection {
            data: vec![1, 3],
            ..base.clone()
        };
        assert!(ensure_section_snapshots_match(&[base], &[changed]).is_err());
    }
}
