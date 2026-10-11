//! Installation of an immutable boot directory before a root switch.

use alloc::{format, vec, vec::Vec};

use axfs_ng_vfs::{MutationCredentials, NodePermission, NodeType, RenameOptions};

use crate::{VfsError, VfsResult, file::File, highlevel::FsContext};

/// Replaces one boot-owned directory as a complete package.
///
/// The caller must exclude other directory writers until this returns. Files
/// outside `path` are untouched. A missing source preserves the installed
/// package; an empty source publishes an empty directory. The validator sees
/// the staged path and must inspect all files required by the new package.
/// Ext4 publishes using EXCHANGE. Other filesystems use a recoverable backup
/// sequence, which does not provide power-loss atomicity.
pub fn install_directory(
    source: &FsContext,
    target: &FsContext,
    path: &str,
    validate: impl Fn(&FsContext, &str) -> VfsResult<()>,
) -> VfsResult<bool> {
    install_with_flush(source, target, path, validate, &flush)
}

fn install_with_flush(
    source: &FsContext,
    target: &FsContext,
    path: &str,
    validate: impl Fn(&FsContext, &str) -> VfsResult<()>,
    flush: &dyn Fn(&FsContext) -> VfsResult<()>,
) -> VfsResult<bool> {
    if !path.starts_with('/')
        || path.ends_with('/')
        || path.split('/').any(|p| matches!(p, "." | ".."))
    {
        return Err(VfsError::InvalidInput);
    }
    let stage = format!("{path}.new");
    let backup = format!("{path}.old");
    recover(target, path, &stage, &backup, &validate, flush)?;
    if !exists(source, path)? {
        return Ok(false);
    }
    if target.root_dir().is_readonly() {
        return Err(VfsError::ReadOnlyFilesystem);
    }
    let (parent, _) = path.rsplit_once('/').ok_or(VfsError::InvalidInput)?;
    mkdir_parents(target, parent)?;
    let result = (|| {
        copy_tree(source, path, target, &stage)?;
        validate(target, &stage)?;
        flush(target)?;
        publish(target, path, &stage, &backup, flush)?;
        Ok(true)
    })();
    if result.is_err() {
        // A failed staging operation never modifies the installed package.
        // Keep any cleanup failure visible for recovery on the next boot.
        if let Err(error) = remove_tree(target, &stage) {
            log::warn!("boot directory staging cleanup failed: {error:?}");
        }
    }
    result
}

pub(crate) fn exists(context: &FsContext, path: &str) -> VfsResult<bool> {
    match context.resolve_no_follow(path) {
        Ok(_) => Ok(true),
        Err(VfsError::NotFound) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn flush(context: &FsContext) -> VfsResult<()> {
    #[cfg(feature = "vfs")]
    crate::file::sync_filesystem_cached_files(context.root_dir().filesystem())?;
    context.root_dir().filesystem().flush()
}

pub(crate) fn rename(
    context: &FsContext,
    from: &str,
    to: &str,
    options: RenameOptions,
) -> VfsResult<()> {
    context.rename_with_options(from, to, options, &MutationCredentials::root())
}

pub(crate) fn recover(
    target: &FsContext,
    path: &str,
    stage: &str,
    backup: &str,
    validate: &impl Fn(&FsContext, &str) -> VfsResult<()>,
    flush: &dyn Fn(&FsContext) -> VfsResult<()>,
) -> VfsResult<()> {
    if exists(target, backup)? {
        if exists(target, path)? {
            if validate(target, path).is_err() {
                remove_tree(target, path)?;
                rename(target, backup, path, RenameOptions::NO_REPLACE)?;
            } else {
                remove_tree(target, backup)?;
            }
        } else {
            rename(target, backup, path, RenameOptions::NO_REPLACE)?;
        }
        flush(target)?;
    }
    remove_tree(target, stage)
}

fn publish(
    target: &FsContext,
    path: &str,
    stage: &str,
    backup: &str,
    flush: &dyn Fn(&FsContext) -> VfsResult<()>,
) -> VfsResult<()> {
    let installed = exists(target, path)?;
    let exchange = installed && target.root_dir().filesystem().name() == "ext4";
    if exchange {
        rename(target, stage, path, RenameOptions::EXCHANGE)?;
    } else {
        if installed {
            rename(target, path, backup, RenameOptions::NO_REPLACE)?;
            if let Err(error) = flush(target) {
                rename(target, backup, path, RenameOptions::NO_REPLACE)?;
                return Err(error);
            }
        }
        if let Err(error) = rename(target, stage, path, RenameOptions::NO_REPLACE) {
            if installed {
                rename(target, backup, path, RenameOptions::NO_REPLACE)?;
            }
            return Err(error);
        }
    }
    if let Err(error) = flush(target) {
        if exchange {
            rename(target, stage, path, RenameOptions::EXCHANGE)?;
        } else {
            rename(target, path, stage, RenameOptions::NO_REPLACE)?;
            if installed {
                rename(target, backup, path, RenameOptions::NO_REPLACE)?;
            }
        }
        flush(target)?;
        return Err(error);
    }
    // Publication is durable. Leftover old directories can be removed during
    // the next recovery without making a committed package fail to boot.
    let old = if exchange { stage } else { backup };
    if let Err(error) = remove_tree(target, old).and_then(|()| flush(target)) {
        log::warn!("published boot directory; old package cleanup deferred: {error:?}");
    }
    Ok(())
}

pub(crate) fn mkdir_parents(context: &FsContext, path: &str) -> VfsResult<()> {
    let mut current = alloc::string::String::new();
    for component in path.split('/').filter(|part| !part.is_empty()) {
        current.push('/');
        current.push_str(component);
        if !exists(context, &current)? {
            context.create_dir(
                &current,
                NodePermission::from_bits_truncate(0o755),
                0,
                0,
                &MutationCredentials::root(),
            )?;
        } else {
            context.resolve(&current)?.check_is_dir()?;
        }
    }
    Ok(())
}

pub(crate) fn child_names(
    context: &FsContext,
    path: &str,
) -> VfsResult<Vec<alloc::string::String>> {
    context
        .read_dir(path)?
        .filter_map(|entry| match entry {
            Ok(entry) if matches!(entry.name.as_str(), "." | "..") => None,
            Ok(entry) => Some(Ok(entry.name)),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub(crate) fn copy_tree(
    source: &FsContext,
    from: &str,
    target: &FsContext,
    to: &str,
) -> VfsResult<()> {
    let node = source.resolve_no_follow(from)?;
    match node.node_type() {
        NodeType::Directory => {
            target.create_dir(
                to,
                NodePermission::from_bits_truncate(0o755),
                0,
                0,
                &MutationCredentials::root(),
            )?;
            for name in child_names(source, from)? {
                copy_tree(
                    source,
                    &format!("{from}/{name}"),
                    target,
                    &format!("{to}/{name}"),
                )?;
            }
            Ok(())
        }
        NodeType::RegularFile => {
            let input = File::open(source, from)?;
            let output = File::create(target, to)?;
            let mut buffer = vec![0; crate::os::memory::PAGE_SIZE];
            let mut offset = 0;
            loop {
                let count = input.read_at(&mut buffer[..], offset)?;
                if count == 0 {
                    break;
                }
                let mut written = 0;
                while written < count {
                    let count_written =
                        output.write_at(&buffer[written..count], offset + written as u64)?;
                    if count_written == 0 {
                        return Err(VfsError::Io);
                    }
                    written += count_written;
                }
                offset += count as u64;
            }
            output.sync(false)
        }
        _ => Err(VfsError::InvalidData),
    }
}

pub(crate) fn remove_tree(context: &FsContext, path: &str) -> VfsResult<()> {
    if !exists(context, path)? {
        return Ok(());
    }
    if context.resolve_no_follow(path)?.node_type() == NodeType::Directory {
        for name in child_names(context, path)? {
            remove_tree(context, &format!("{path}/{name}"))?;
        }
        context.remove_dir(path, &MutationCredentials::root())
    } else {
        context.remove_file(path, &MutationCredentials::root())
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use axfs_ng_vfs::Mountpoint;

    use super::*;
    use crate::MemoryFs;

    fn context() -> FsContext {
        FsContext::new(Mountpoint::new_root(&MemoryFs::new()).root_location())
    }
    fn write(context: &FsContext, path: &str, data: &[u8]) {
        File::create(context, path)
            .unwrap()
            .write_at(data, 0)
            .unwrap();
    }

    #[test]
    fn directory_install_replaces_recovers_and_preserves_on_validation_failure() {
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            let source = context();
            let target = context();
            mkdir_parents(&source, "/guest/builtin").unwrap();
            mkdir_parents(&target, "/guest/builtin").unwrap();
            write(&target, "/guest/builtin/old-only", b"old");
            write(&source, "/guest/builtin/kernel", b"new");
            assert_eq!(
                install_directory(&source, &target, "/guest/builtin", |_, _| Err(
                    VfsError::InvalidData
                )),
                Err(VfsError::InvalidData)
            );
            assert!(target.resolve("/guest/builtin/old-only").is_ok());
            install_directory(&source, &target, "/guest/builtin", |_, _| Ok(())).unwrap();
            assert!(target.resolve("/guest/builtin/old-only").is_err());
            let mut bytes = [0; 3];
            File::open(&target, "/guest/builtin/kernel")
                .unwrap()
                .read_at(&mut bytes[..], 0)
                .unwrap();
            assert_eq!(&bytes, b"new");
            // An interruption after backing up the old directory must restore
            // it before a later archive without a package preserves it.
            rename(
                &target,
                "/guest/builtin",
                "/guest/builtin.old",
                RenameOptions::NO_REPLACE,
            )
            .unwrap();
            mkdir_parents(&target, "/guest/builtin.new").unwrap();
            write(&target, "/guest/builtin.new/partial", b"incomplete");
            let absent = context();
            assert!(!install_directory(&absent, &target, "/guest/builtin", |_, _| Ok(())).unwrap());
            assert!(target.resolve("/guest/builtin/kernel").is_ok());
            assert!(target.resolve("/guest/builtin.new").is_err());
            remove_tree(&source, "/guest/builtin/kernel").unwrap();
            install_directory(&source, &target, "/guest/builtin", |_, _| Ok(())).unwrap();
            assert!(child_names(&target, "/guest/builtin").unwrap().is_empty());
            let full = FsContext::new(
                Mountpoint::new_root(&MemoryFs::new_with_size_limit(4)).root_location(),
            );
            mkdir_parents(&full, "/guest/builtin").unwrap();
            write(&full, "/guest/builtin/old-only", b"old");
            write(&source, "/guest/builtin/kernel", b"new");
            assert_eq!(
                install_directory(&source, &full, "/guest/builtin", |_, _| Ok(())),
                Err(VfsError::StorageFull)
            );
            assert!(full.resolve("/guest/builtin/old-only").is_ok());
            target.root_dir().mountpoint().set_readonly(true);
            assert_eq!(
                install_directory(&source, &target, "/guest/builtin", |_, _| Ok(())),
                Err(VfsError::ReadOnlyFilesystem)
            );
        });
    }

    #[test]
    fn failed_publication_flush_restores_the_installed_directory() {
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            for fail_at in [2, 3] {
                let source = context();
                mkdir_parents(&source, "/guest/builtin").unwrap();
                write(&source, "/guest/builtin/new", b"new");
                let target = context();
                let calls = Cell::new(0);
                let failing_flush = |context: &FsContext| {
                    flush(context)?;
                    calls.set(calls.get() + 1);
                    if calls.get() == fail_at {
                        Err(VfsError::Io)
                    } else {
                        Ok(())
                    }
                };
                mkdir_parents(&target, "/guest/builtin").unwrap();
                write(&target, "/guest/builtin/old", b"old");
                assert_eq!(
                    install_with_flush(
                        &source,
                        &target,
                        "/guest/builtin",
                        |_, _| Ok(()),
                        &failing_flush
                    ),
                    Err(VfsError::Io)
                );
                assert_eq!(target.read("/guest/builtin/old").unwrap(), b"old");
                assert!(target.resolve("/guest/builtin/new").is_err());
                assert!(target.resolve("/guest/builtin.new").is_err());
                assert!(target.resolve("/guest/builtin.old").is_err());
            }
        });
    }
}
