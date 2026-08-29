//! Materialization of per-boot host assets into the StarryOS tmpfs.

use alloc::{format, string::String, vec::Vec};

use ax_fs_ng::VfsError;
use axfs_ng_vfs::{MetadataUpdate, NodePermission, NodeType};
use boot_session_archive::{Archive, ArchiveError, FDT_PROPERTY_NAME, GUEST_ROOT};
use thiserror::Error;

const PRIVATE_DIRECTORY_MODE: NodePermission = NodePermission::from_bits_truncate(0o700);

#[derive(Debug, Error)]
pub(crate) enum BootSessionError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("boot session tmpfs operation failed: {0:?}")]
    Filesystem(VfsError),
}

impl From<VfsError> for BootSessionError {
    fn from(error: VfsError) -> Self {
        Self::Filesystem(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MaterializedBootSession {
    pub(crate) file_count: usize,
    pub(crate) byte_count: usize,
}

/// Validates and copies the complete boot session archive into `/tmp`.
///
/// The archive comes from a per-run DTB, not the repository DTB. All output is
/// rooted in a newly created private tmpfs directory, and a partial extraction
/// is rolled back before an error is returned.
pub(crate) fn materialize() -> Result<Option<MaterializedBootSession>, BootSessionError> {
    let Some(bytes) = ax_hal::dtb::get_chosen_property(FDT_PROPERTY_NAME) else {
        return Ok(None);
    };
    let archive = Archive::parse(bytes)?;
    let fs_context = ax_fs_ng::vfs::current_fs_context();
    let fs = fs_context.lock();
    let mut created_files = Vec::with_capacity(archive.len());
    let mut created_directories = Vec::new();
    let result = (|| {
        fs.create_dir(GUEST_ROOT, PRIVATE_DIRECTORY_MODE, 0, 0)?;
        created_directories.push(String::from(GUEST_ROOT));

        let mut byte_count = 0_usize;
        for entry in archive.entries() {
            let destination = format!("{GUEST_ROOT}/{}", entry.path());
            create_parent_directories(
                &fs,
                entry.path(),
                &mut created_directories,
            )?;
            fs.write(&destination, entry.contents())?;
            created_files.push(destination.clone());
            fs.resolve(&destination)?.update_metadata(MetadataUpdate {
                mode: Some(NodePermission::from_bits_truncate(entry.mode())),
                ..Default::default()
            })?;
            byte_count += entry.contents().len();
        }
        Ok(MaterializedBootSession {
            file_count: archive.len(),
            byte_count,
        })
    })();

    match result {
        Ok(summary) => Ok(Some(summary)),
        Err(error) => {
            rollback(&fs, &created_files, &created_directories);
            Err(error)
        }
    }
}

fn create_parent_directories(
    fs: &ax_fs_ng::vfs::FsContext,
    relative_path: &str,
    created_directories: &mut Vec<String>,
) -> Result<(), BootSessionError> {
    let Some((parents, _)) = relative_path.rsplit_once('/') else {
        return Ok(());
    };
    let mut current = String::from(GUEST_ROOT);
    for component in parents.split('/') {
        current.push('/');
        current.push_str(component);
        match fs.resolve(&current) {
            Ok(location) if location.metadata()?.node_type == NodeType::Directory => {}
            Ok(_) => return Err(BootSessionError::Filesystem(VfsError::NotADirectory)),
            Err(VfsError::NotFound) => {
                fs.create_dir(&current, PRIVATE_DIRECTORY_MODE, 0, 0)?;
                created_directories.push(current.clone());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn rollback(
    fs: &ax_fs_ng::vfs::FsContext,
    created_files: &[String],
    created_directories: &[String],
) {
    for path in created_files.iter().rev() {
        let _ = fs.remove_file(path);
    }
    for path in created_directories.iter().rev() {
        let _ = fs.remove_dir(path);
    }
}
