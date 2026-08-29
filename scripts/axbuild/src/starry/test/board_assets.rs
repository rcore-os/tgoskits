use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use boot_session_archive::{ArchiveInput, FDT_PROPERTY_NAME, GUEST_ROOT};
use ostool::board::{BoardRunRequest, RunBoardOptions, config::BoardRunConfig};
use zeroize::Zeroizing;

use crate::{
    starry::test::starry_case_asset_config,
    test::{
        build::prepare_c_case_overlay_sync,
        case::{TestQemuCase, board_case_asset_layout},
    },
};

const C_SOURCE_DIR: &str = "c";
const CMAKE_PROJECT_FILE: &str = "CMakeLists.txt";

#[derive(Debug)]
pub(in crate::starry) struct PreparedBoardSessionAssets {
    pub(crate) root: PathBuf,
    pub(crate) relative_paths: Vec<PathBuf>,
    cleanup_root: PathBuf,
    pub(crate) inject_boot_entropy: bool,
}

pub(in crate::starry) struct SessionRunDirectoryGuard {
    path: PathBuf,
    preserve: bool,
}

impl SessionRunDirectoryGuard {
    pub(in crate::starry) fn new(path: PathBuf) -> Self {
        Self {
            path,
            preserve: false,
        }
    }

    pub(in crate::starry) fn preserve(mut self) -> PathBuf {
        self.preserve = true;
        self.path.clone()
    }
}

impl Drop for SessionRunDirectoryGuard {
    fn drop(&mut self) {
        if !self.preserve {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl Drop for PreparedBoardSessionAssets {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.cleanup_root);
    }
}

impl PreparedBoardSessionAssets {
    pub(crate) fn new(
        root: PathBuf,
        relative_paths: Vec<PathBuf>,
        cleanup_root: PathBuf,
        inject_boot_entropy: bool,
    ) -> Self {
        Self {
            root,
            relative_paths,
            cleanup_root,
            inject_boot_entropy,
        }
    }

    pub(crate) fn prepare_boot_data(
        &self,
        board_config: &mut ostool::board::config::BoardRunConfig,
    ) -> anyhow::Result<()> {
        if !self.inject_boot_entropy && self.relative_paths.is_empty() {
            return Ok(());
        }
        let source = board_config
            .dtb_file
            .as_deref()
            .map(Path::new)
            .ok_or_else(|| anyhow::anyhow!("boot entropy injection requires dtb_file"))?;
        let mut fdt = fdt_edit::Fdt::from_bytes(
            &fs::read(source).with_context(|| format!("failed to read {}", source.display()))?,
        )?;
        let chosen_id = fdt
            .get_by_path("/chosen")
            .map(|node| node.id())
            .ok_or_else(|| anyhow::anyhow!("DTB does not contain /chosen"))?;
        if self.inject_boot_entropy {
            let mut seed = [0; 32];
            getrandom::fill(&mut seed).context("host OS random source is unavailable")?;
            fdt.node_mut(chosen_id)
                .expect("chosen node id came from the same FDT")
                .set_property(fdt_edit::Property::new("rng-seed", seed.to_vec()));
        }
        if !self.relative_paths.is_empty() {
            let files = self.read_archive_inputs()?;
            let archive = Zeroizing::new(
                boot_session_archive::encode(files.iter().map(|file| ArchiveInput {
                    path: &file.path,
                    mode: file.mode,
                    contents: &file.contents,
                }))
                .context("failed to encode boot session files")?,
            );
            fdt.node_mut(chosen_id)
                .expect("chosen node id came from the same FDT")
                .set_property(fdt_edit::Property::new(
                    FDT_PROPERTY_NAME,
                    archive.as_slice().to_vec(),
                ));
            localize_session_file_placeholders(board_config, &self.relative_paths)?;
            board_config.session_files.clear();
        }
        let destination = self.cleanup_root.join("boot-session.dtb");
        let bytes = fdt.encode();
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&destination)
            .with_context(|| format!("failed to open {}", destination.display()))?;
        file.write_all(bytes.as_ref())
            .with_context(|| format!("failed to write {}", destination.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to persist {}", destination.display()))?;
        board_config.dtb_file = Some(destination.to_string_lossy().into_owned());
        Ok(())
    }

    fn read_archive_inputs(&self) -> anyhow::Result<Vec<OwnedArchiveInput>> {
        self.relative_paths
            .iter()
            .map(|relative_path| {
                validate_relative_path(relative_path)?;
                let path = relative_path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("boot session file path is not valid UTF-8"))?;
                let source = self.root.join(relative_path);
                let metadata = fs::symlink_metadata(&source)
                    .with_context(|| format!("failed to inspect boot session file `{path}`"))?;
                ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "boot session entry `{path}` is not a regular file"
                );
                Ok(OwnedArchiveInput {
                    path: path.to_owned(),
                    mode: private_guest_mode(&metadata),
                    contents: Zeroizing::new(
                        fs::read(&source).with_context(|| {
                            format!("failed to read boot session file `{path}`")
                        })?,
                    ),
                })
            })
            .collect()
    }

    pub(crate) fn attach_to_board_request(
        &self,
        board_config: BoardRunConfig,
        options: RunBoardOptions,
    ) -> anyhow::Result<BoardRunRequest> {
        ensure!(
            board_config.session_files.is_empty(),
            "boot session files were not prepared before creating the board request"
        );
        Ok(BoardRunRequest::new(board_config, options))
    }
}

struct OwnedArchiveInput {
    path: String,
    mode: u16,
    contents: Zeroizing<Vec<u8>>,
}

fn private_guest_mode(metadata: &fs::Metadata) -> u16 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 != 0 {
            return 0o700;
        }
    }
    0o600
}

fn localize_session_file_placeholders(
    board_config: &mut BoardRunConfig,
    relative_paths: &[PathBuf],
) -> anyhow::Result<()> {
    let Some(command) = board_config.shell_init_cmd.as_deref() else {
        return Ok(());
    };
    const PREFIX: &str = "${sessionFile:";
    let mut remaining = command;
    let mut localized = String::with_capacity(command.len());
    while let Some(start) = remaining.find(PREFIX) {
        localized.push_str(&remaining[..start]);
        let placeholder = &remaining[start + PREFIX.len()..];
        let end = placeholder.find('}').ok_or_else(|| {
            anyhow::anyhow!("unterminated sessionFile placeholder in shell_init_cmd")
        })?;
        let relative = Path::new(&placeholder[..end]);
        validate_relative_path(relative)?;
        ensure!(
            relative_paths.iter().any(|available| available == relative),
            "shell_init_cmd references unavailable boot session file `{}`",
            relative.display()
        );
        localized.push_str(GUEST_ROOT);
        localized.push('/');
        localized.push_str(&placeholder[..end]);
        remaining = &placeholder[end + 1..];
    }
    localized.push_str(remaining);
    board_config.shell_init_cmd = Some(localized);
    Ok(())
}

pub(crate) async fn prepare_board_session_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
    case_dir: &Path,
    board_config_path: &Path,
    declared_session_files: &[PathBuf],
) -> anyhow::Result<Option<PreparedBoardSessionAssets>> {
    let session_env =
        crate::starry::session_env::SessionEnvConfig::load(case_dir, board_config_path)?;
    if let Some(session_env) = &session_env {
        session_env.validate_environment()?;
    }
    let cmake_project = case_dir.join(C_SOURCE_DIR).join(CMAKE_PROJECT_FILE);
    if !cmake_project.is_file() && session_env.is_none() && declared_session_files.is_empty() {
        return Ok(None);
    }
    for unsupported_dir in ["sh", "python"] {
        ensure!(
            !case_dir.join(unsupported_dir).exists(),
            "board case `{case_name}` combines C assets with unsupported `{unsupported_dir}` \
             assets"
        );
    }

    let rootfs = if cmake_project.is_file() {
        Some(crate::starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?)
    } else {
        None
    };
    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case_name = case_name.to_string();
    let case_dir = case_dir.to_path_buf();
    let board_config_path = board_config_path.to_path_buf();
    let declared_session_files = declared_session_files.to_vec();
    let session_env = session_env.unwrap_or_default();

    let assets = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let layout = board_case_asset_layout(&workspace_root, &target, &case_name)?;
        let cleanup = SessionRunDirectoryGuard::new(layout.run_dir.clone());
        let case = TestQemuCase {
            name: case_name.clone(),
            display_name: case_name,
            case_dir: case_dir.clone(),
            qemu_config_path: board_config_path,
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: None,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        };
        if let Some(rootfs) = &rootfs {
            prepare_c_case_overlay_sync(
                &arch,
                &case,
                rootfs,
                &layout,
                &starry_case_asset_config(),
            )?;
        } else {
            crate::test::case::reset_dir(&layout.overlay_dir)?;
        }
        copy_declared_session_files(&case_dir, &layout.overlay_dir, &declared_session_files)?;
        session_env.materialize(&layout.overlay_dir)?;
        let relative_paths = collect_upload_paths(&layout.overlay_dir)?;
        Ok(PreparedBoardSessionAssets::new(
            layout.overlay_dir,
            relative_paths,
            cleanup.preserve(),
            session_env.inject_boot_entropy,
        ))
    })
    .await
    .context("Starry board session asset task failed")??;
    Ok(Some(assets))
}

pub(in crate::starry) fn copy_declared_session_files(
    case_dir: &Path,
    upload_root: &Path,
    relative_paths: &[PathBuf],
) -> anyhow::Result<()> {
    let canonical_case_dir = case_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve board case directory {}",
            case_dir.display()
        )
    })?;
    let mut copied = BTreeSet::new();

    for relative_path in relative_paths {
        validate_relative_path(relative_path)?;
        ensure!(
            copied.insert(relative_path.clone()),
            "duplicate session file path `{}`",
            relative_path.display()
        );
        let source = case_dir.join(relative_path);
        let metadata = fs::symlink_metadata(&source).with_context(|| {
            format!(
                "failed to inspect declared session file `{}`",
                source.display()
            )
        })?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "declared session file `{}` must not be a symbolic link",
            relative_path.display()
        );
        ensure!(
            metadata.is_file(),
            "declared session file `{}` is not a regular file",
            relative_path.display()
        );
        let canonical_source = source.canonicalize().with_context(|| {
            format!(
                "failed to resolve declared session file `{}`",
                source.display()
            )
        })?;
        ensure!(
            canonical_source.starts_with(&canonical_case_dir),
            "declared session file `{}` escapes the board case directory",
            relative_path.display()
        );

        let destination = upload_root.join(relative_path);
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!(
                "declared session file `{}` conflicts with a CMake install product",
                relative_path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect upload destination `{}`",
                        destination.display()
                    )
                });
            }
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::copy(&source, &destination).with_context(|| {
            format!(
                "failed to copy declared session file `{}` to `{}`",
                source.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

pub(in crate::starry) fn collect_upload_paths(upload_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut pending = vec![upload_root.to_path_buf()];
    let mut relative_paths = Vec::new();

    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("failed to read upload directory {}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read upload directory {}", directory.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect upload entry {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "board session upload entry `{}` must not be a symbolic link",
                    path.display()
                );
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            ensure!(
                metadata.is_file(),
                "board session upload entry `{}` is not a regular file",
                path.display()
            );
            relative_paths.push(
                path.strip_prefix(upload_root)
                    .expect("upload entry is below upload root")
                    .to_path_buf(),
            );
        }
    }

    relative_paths.sort();
    ensure!(
        !relative_paths.is_empty(),
        "board CMake install produced no files in upload root `{}`",
        upload_root.display()
    );
    Ok(relative_paths)
}

pub(in crate::starry) fn validate_relative_path(path: &Path) -> anyhow::Result<()> {
    ensure!(
        !path.as_os_str().is_empty() && !path.is_absolute(),
        "session file path `{}` must be a non-empty relative path",
        path.display()
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "session file path `{}` must be a normalized relative path",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{
        PreparedBoardSessionAssets, SessionRunDirectoryGuard, collect_upload_paths,
        copy_declared_session_files,
    };

    #[test]
    fn board_request_borrows_assets_without_dropping_the_boot_root() {
        let root = tempdir().unwrap();
        let run_dir = root.path().join("run");
        let upload_root = run_dir.join("upload");
        let helper = upload_root.join("bin/helper");
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        fs::write(&helper, b"helper").unwrap();
        let assets = PreparedBoardSessionAssets::new(
            upload_root,
            vec![PathBuf::from("bin/helper")],
            run_dir.clone(),
            false,
        );

        let _request = assets
            .attach_to_board_request(
                ostool::board::config::BoardRunConfig::default(),
                ostool::board::RunBoardOptions {
                    board_type: None,
                    server: None,
                    port: None,
                },
            )
            .unwrap();

        assert!(helper.is_file());
        drop(assets);
        assert!(!run_dir.exists());
    }

    #[test]
    fn failed_session_preparation_removes_its_run_directory() {
        let root = tempdir().unwrap();
        let run_dir = root.path().join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("credential"), b"secret").unwrap();

        drop(SessionRunDirectoryGuard::new(run_dir.clone()));

        assert!(!run_dir.exists());
    }

    #[test]
    fn boot_entropy_uses_a_session_copy_and_is_removed_with_the_session() {
        let root = tempdir().unwrap();
        let source = root.path().join("board.dtb");
        let run_dir = root.path().join("run");
        let upload_root = run_dir.join("upload");
        fs::create_dir_all(&upload_root).unwrap();
        fs::write(upload_root.join("credential"), b"session-secret").unwrap();
        let mut source_fdt = fdt_edit::Fdt::new();
        source_fdt.add_node(source_fdt.root_id(), fdt_edit::Node::new("chosen"));
        let source_bytes = source_fdt.encode();
        fs::write(&source, source_bytes.as_ref()).unwrap();

        let mut board_config = ostool::board::config::BoardRunConfig {
            dtb_file: Some(source.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let assets = PreparedBoardSessionAssets::new(
            upload_root,
            vec![PathBuf::from("credential")],
            run_dir.clone(),
            true,
        );
        board_config.session_files = vec![PathBuf::from("credential")];
        board_config.shell_init_cmd =
            Some("helper '${sessionFile:credential}' '${boardServerIp}'".to_owned());
        assets.prepare_boot_data(&mut board_config).unwrap();

        let temporary_dtb = PathBuf::from(board_config.dtb_file.as_ref().unwrap());
        assert_eq!(temporary_dtb, run_dir.join("boot-session.dtb"));
        assert!(board_config.session_files.is_empty());
        assert_eq!(
            board_config.shell_init_cmd.as_deref(),
            Some("helper '/tmp/starry-session/credential' '${boardServerIp}'")
        );
        assert_eq!(fs::read(&source).unwrap(), source_bytes.as_ref());
        let injected = fdt_edit::Fdt::from_bytes(&fs::read(&temporary_dtb).unwrap()).unwrap();
        assert_eq!(
            injected
                .get_by_path("/chosen")
                .unwrap()
                .as_node()
                .get_property("rng-seed")
                .unwrap()
                .data
                .len(),
            32
        );
        let archive = injected
            .get_by_path("/chosen")
            .unwrap()
            .as_node()
            .get_property(boot_session_archive::FDT_PROPERTY_NAME)
            .expect("board session files must be available before networking starts");
        let archive = boot_session_archive::Archive::parse(&archive.data).unwrap();
        let entry = archive.entries().next().unwrap();
        assert_eq!(entry.path(), "credential");
        assert_eq!(entry.mode(), 0o600);
        assert_eq!(entry.contents(), b"session-secret");

        drop(assets);
        assert!(!run_dir.exists());
        assert!(source.exists());
    }

    #[test]
    fn upload_paths_are_sorted_and_keep_nested_relative_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("tools/network")).unwrap();
        fs::write(root.path().join("tools/network/probe"), b"probe").unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::write(root.path().join("bin/app"), b"app").unwrap();

        assert_eq!(
            collect_upload_paths(root.path()).unwrap(),
            [
                PathBuf::from("bin/app"),
                PathBuf::from("tools/network/probe")
            ]
        );
    }

    #[test]
    fn upload_root_must_contain_at_least_one_regular_file() {
        let root = tempdir().unwrap();

        let error = collect_upload_paths(root.path()).unwrap_err();

        assert!(error.to_string().contains("no files"));
    }

    #[cfg(unix)]
    #[test]
    fn upload_root_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        fs::write(root.path().join("app"), b"app").unwrap();
        symlink("app", root.path().join("alias")).unwrap();

        let error = collect_upload_paths(root.path()).unwrap_err();

        assert!(error.to_string().contains("symbolic link"));
    }

    #[test]
    fn declared_session_files_are_copied_without_renaming() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("case");
        let upload_root = root.path().join("upload");
        fs::create_dir_all(case_dir.join("fixtures")).unwrap();
        fs::create_dir_all(&upload_root).unwrap();
        fs::write(case_dir.join("fixtures/input.json"), b"input").unwrap();

        copy_declared_session_files(
            &case_dir,
            &upload_root,
            &[PathBuf::from("fixtures/input.json")],
        )
        .unwrap();

        assert_eq!(
            fs::read(upload_root.join("fixtures/input.json")).unwrap(),
            b"input"
        );
    }

    #[test]
    fn declared_session_files_reject_collisions_and_invalid_paths() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("case");
        let upload_root = root.path().join("upload");
        fs::create_dir_all(case_dir.join("bin")).unwrap();
        fs::create_dir_all(upload_root.join("bin")).unwrap();
        fs::write(case_dir.join("bin/app"), b"source").unwrap();
        fs::write(upload_root.join("bin/app"), b"built").unwrap();

        let collision =
            copy_declared_session_files(&case_dir, &upload_root, &[PathBuf::from("bin/app")])
                .unwrap_err();
        assert!(collision.to_string().contains("conflicts"));

        let escape =
            copy_declared_session_files(&case_dir, &upload_root, &[PathBuf::from("../escape")])
                .unwrap_err();
        assert!(escape.to_string().contains("relative"));
    }
}
