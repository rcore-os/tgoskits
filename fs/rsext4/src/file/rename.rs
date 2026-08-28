use ::alloc::collections::BTreeSet;

use super::*;
use crate::dir::{FileName, insert_dir_entry_raw};

// Linux ext4_rename reserves two DATA_TRANS_BLOCKS owners, one index-growth
// allowance, and two inode records. Writable quota is not implemented yet.
const RENAME_TRANSACTION_CREDITS: usize = 2 * 24 + 12 + 2;

// RENAME_EXCHANGE can update directory indices on both sides.
const RENAME_EXCHANGE_TRANSACTION_CREDITS: usize = 2 * 24 + 2 * 12 + 2;

/// Filesystem-level rename behavior selected by a VFS.
///
/// The representation is private so invalid Linux flag combinations cannot be
/// constructed. `NOREPLACE` is orthogonal to `WHITEOUT`, while `EXCHANGE`
/// excludes both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameOptions {
    no_replace: bool,
    exchange: bool,
    whiteout: bool,
}

impl RenameOptions {
    pub const REPLACE: Self = Self::new(false, false, false);
    pub const NO_REPLACE: Self = Self::new(true, false, false);
    pub const EXCHANGE: Self = Self::new(false, true, false);
    pub const WHITEOUT: Self = Self::new(false, false, true);
    pub const WHITEOUT_NO_REPLACE: Self = Self::new(true, false, true);

    const fn new(no_replace: bool, exchange: bool, whiteout: bool) -> Self {
        Self {
            no_replace,
            exchange,
            whiteout,
        }
    }

    pub const fn no_replace(self) -> bool {
        self.no_replace
    }

    pub const fn exchange(self) -> bool {
        self.exchange
    }

    pub const fn whiteout(self) -> bool {
        self.whiteout
    }
}

impl Default for RenameOptions {
    fn default() -> Self {
        Self::REPLACE
    }
}

/// Result of a rename that may have detached an existing target inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[must_use]
pub struct RenameOutcome {
    pub replaced: Option<UnlinkOutcome>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RenameEntryRequest<'a> {
    pub old_parent: InodeNumber,
    pub old_name: FileName<'a>,
    pub new_parent: InodeNumber,
    pub new_name: FileName<'a>,
    pub options: RenameOptions,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedRename {
    old_parent_inode: Ext4Inode,
    source: ParentDirEntry,
    source_inode: Ext4Inode,
    new_parent_inode: Ext4Inode,
    destination: Option<ParentDirEntry>,
    destination_inode: Option<Ext4Inode>,
}

fn optional_entry<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    parent: InodeNumber,
    parent_inode: &Ext4Inode,
    name: FileName<'_>,
) -> Ext4Result<Option<ParentDirEntry>> {
    match find_named_entry_in_parent(fs, device, parent, parent_inode, name.as_bytes()) {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if error.kind() == Ext4ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn ensure_directory_move_is_acyclic<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    moved_directory: InodeNumber,
    new_parent: InodeNumber,
) -> Ext4Result<()> {
    let mut current = new_parent;
    let mut visited = BTreeSet::new();
    loop {
        if current == moved_directory {
            return Err(Ext4Error::invalid_input().with_operation("rename:directory_cycle"));
        }
        if current == fs.root_inode {
            return Ok(());
        }
        if !visited.insert(current) {
            return Err(Ext4Error::corrupted().with_operation("rename:parent_cycle"));
        }
        let inode = fs.get_inode_by_num(device, current)?;
        if !inode.is_dir() {
            return Err(Ext4Error::corrupted().with_operation("rename:parent_not_directory"));
        }
        let parent = find_named_entry_in_parent(fs, device, current, &inode, b"..")?.ino;
        if parent == current {
            return Err(Ext4Error::corrupted().with_operation("rename:self_parent"));
        }
        current = parent;
    }
}

fn rewrite_directory_parent<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    directory: InodeNumber,
    expected_parent: InodeNumber,
    new_parent: InodeNumber,
) -> Ext4Result<()> {
    let inode = fs.get_inode_by_num(device, directory)?;
    let parent_entry = find_named_entry_in_parent(fs, device, directory, &inode, b"..")?;
    if parent_entry.ino != expected_parent {
        return Err(Ext4Error::corrupted().with_operation("rename:dotdot_parent"));
    }
    replace_named_entry_at(
        fs,
        device,
        directory,
        &inode,
        parent_entry,
        b"..",
        DentryReplacement {
            inode: new_parent,
            file_type: parent_entry.file_type,
        },
    )?;
    fs.touch_inode_ctime_for_link_change(device, directory)
}

fn replacement_link_count<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    target: ParentDirEntry,
    target_inode: &Ext4Inode,
) -> Ext4Result<u16> {
    let links = if target_inode.is_dir() {
        0
    } else {
        target_inode.decremented_links_count()?
    };
    if links == 0 {
        preflight_inode_free(fs, device, target.ino, target_inode)?;
    }
    Ok(links)
}

fn publish_replacement_unlink<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    target: ParentDirEntry,
    remaining_links: u16,
) -> Ext4Result<UnlinkOutcome> {
    fs.set_inode_links_count(device, target.ino, remaining_links)?;
    if remaining_links == 0 {
        fs.add_orphan(device, target.ino)?;
    }
    Ok(UnlinkOutcome {
        inode: target.ino,
        remaining_links,
    })
}

fn parent_links_after_move(
    fs: &Ext4FileSystem,
    old_parent: InodeNumber,
    old_parent_inode: &Ext4Inode,
    new_parent: InodeNumber,
    new_parent_inode: &Ext4Inode,
    source_is_directory: bool,
    replaces_directory: bool,
) -> Ext4Result<(Option<u16>, Option<u16>)> {
    if !source_is_directory {
        return Ok((None, None));
    }
    if old_parent == new_parent {
        return if replaces_directory {
            Ok((Some(old_parent_inode.decremented_links_count()?), None))
        } else {
            Ok((None, None))
        };
    }

    let old_links = old_parent_inode.decremented_links_count()?;
    let new_links = if replaces_directory {
        None
    } else {
        let dir_nlink = fs
            .superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK);
        Some(new_parent_inode.incremented_links_count(dir_nlink)?)
    };
    Ok((Some(old_links), new_links))
}

fn apply_parent_links<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    old_parent: InodeNumber,
    new_parent: InodeNumber,
    links: (Option<u16>, Option<u16>),
) -> Ext4Result<()> {
    if let Some(old_links) = links.0 {
        fs.set_inode_links_count(device, old_parent, old_links)?;
    }
    if let Some(new_links) = links.1 {
        fs.set_inode_links_count(device, new_parent, new_links)?;
    }
    Ok(())
}

fn replace_or_add_destination<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: RenameEntryRequest<'_>,
    source: ParentDirEntry,
    new_parent_inode: &Ext4Inode,
    destination: Option<ParentDirEntry>,
) -> Ext4Result<()> {
    if let Some(destination) = destination {
        replace_named_entry_at(
            fs,
            device,
            request.new_parent,
            new_parent_inode,
            destination,
            request.new_name.as_bytes(),
            DentryReplacement {
                inode: source.ino,
                file_type: source.file_type,
            },
        )?;
        fs.touch_parent_dir_for_entry_change(device, request.new_parent)
    } else {
        let mut parent = *new_parent_inode;
        insert_dir_entry_raw(
            fs,
            device,
            request.new_parent,
            &mut parent,
            source.ino,
            request.new_name,
            source.file_type,
        )
    }
}

fn rename_replace<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: RenameEntryRequest<'_>,
    resolved: ResolvedRename,
) -> Ext4Result<RenameOutcome> {
    let ResolvedRename {
        old_parent_inode,
        source,
        source_inode,
        new_parent_inode,
        destination,
        destination_inode,
    } = resolved;
    if let Some(target_inode) = destination_inode {
        if source_inode.is_dir() != target_inode.is_dir() {
            return Err(if source_inode.is_dir() {
                Ext4Error::is_dir()
            } else {
                Ext4Error::not_dir()
            });
        }
        if target_inode.is_dir() {
            let target = destination.ok_or_else(Ext4Error::corrupted)?;
            let mut target_copy = target_inode;
            if !is_dir_empty(fs, device, target.ino, &mut target_copy)? {
                return Err(Ext4Error::not_empty());
            }
        }
    }

    if source_inode.is_dir() {
        ensure_directory_move_is_acyclic(fs, device, source.ino, request.new_parent)?;
    }
    let replaces_directory = destination_inode.is_some_and(|inode| inode.is_dir());
    let parent_links = parent_links_after_move(
        fs,
        request.old_parent,
        &old_parent_inode,
        request.new_parent,
        &new_parent_inode,
        source_inode.is_dir(),
        replaces_directory,
    )?;
    let replaced_links = match (destination, destination_inode) {
        (Some(target), Some(target_inode)) => {
            Some(replacement_link_count(fs, device, target, &target_inode)?)
        }
        _ => None,
    };

    replace_or_add_destination(fs, device, request, source, &new_parent_inode, destination)?;
    remove_named_entry_at(
        fs,
        device,
        request.old_parent,
        &old_parent_inode,
        source,
        request.old_name.as_bytes(),
    )?;
    fs.touch_parent_dir_for_entry_change(device, request.old_parent)?;

    if source_inode.is_dir() && request.old_parent != request.new_parent {
        rewrite_directory_parent(
            fs,
            device,
            source.ino,
            request.old_parent,
            request.new_parent,
        )?;
    }
    apply_parent_links(
        fs,
        device,
        request.old_parent,
        request.new_parent,
        parent_links,
    )?;
    fs.touch_inode_ctime_for_link_change(device, source.ino)?;

    let replaced = match (destination, replaced_links) {
        (Some(target), Some(links)) => Some(publish_replacement_unlink(fs, device, target, links)?),
        _ => None,
    };
    Ok(RenameOutcome { replaced })
}

fn exchange_parent_links(
    fs: &Ext4FileSystem,
    old_parent: InodeNumber,
    old_parent_inode: &Ext4Inode,
    new_parent: InodeNumber,
    new_parent_inode: &Ext4Inode,
    source_is_directory: bool,
    target_is_directory: bool,
) -> Ext4Result<(Option<u16>, Option<u16>)> {
    if old_parent == new_parent || source_is_directory == target_is_directory {
        return Ok((None, None));
    }
    let dir_nlink = fs
        .superblock
        .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_DIR_NLINK);
    if source_is_directory {
        Ok((
            Some(old_parent_inode.decremented_links_count()?),
            Some(new_parent_inode.incremented_links_count(dir_nlink)?),
        ))
    } else {
        Ok((
            Some(old_parent_inode.incremented_links_count(dir_nlink)?),
            Some(new_parent_inode.decremented_links_count()?),
        ))
    }
}

fn rename_exchange<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: RenameEntryRequest<'_>,
    resolved: ResolvedRename,
) -> Ext4Result<RenameOutcome> {
    let ResolvedRename {
        old_parent_inode,
        source,
        source_inode,
        new_parent_inode,
        destination,
        destination_inode,
    } = resolved;
    let target = destination.ok_or_else(Ext4Error::not_found)?;
    let target_inode = destination_inode.ok_or_else(Ext4Error::corrupted)?;
    if request.old_parent != request.new_parent {
        if source_inode.is_dir() {
            ensure_directory_move_is_acyclic(fs, device, source.ino, request.new_parent)?;
        }
        if target_inode.is_dir() {
            ensure_directory_move_is_acyclic(fs, device, target.ino, request.old_parent)?;
        }
    }
    let parent_links = exchange_parent_links(
        fs,
        request.old_parent,
        &old_parent_inode,
        request.new_parent,
        &new_parent_inode,
        source_inode.is_dir(),
        target_inode.is_dir(),
    )?;

    replace_named_entry_at(
        fs,
        device,
        request.new_parent,
        &new_parent_inode,
        target,
        request.new_name.as_bytes(),
        DentryReplacement {
            inode: source.ino,
            file_type: source.file_type,
        },
    )?;
    replace_named_entry_at(
        fs,
        device,
        request.old_parent,
        &old_parent_inode,
        source,
        request.old_name.as_bytes(),
        DentryReplacement {
            inode: target.ino,
            file_type: target.file_type,
        },
    )?;
    fs.touch_parent_dir_for_entry_change(device, request.old_parent)?;
    if request.old_parent != request.new_parent {
        fs.touch_parent_dir_for_entry_change(device, request.new_parent)?;
        if source_inode.is_dir() {
            rewrite_directory_parent(
                fs,
                device,
                source.ino,
                request.old_parent,
                request.new_parent,
            )?;
        }
        if target_inode.is_dir() {
            rewrite_directory_parent(
                fs,
                device,
                target.ino,
                request.new_parent,
                request.old_parent,
            )?;
        }
    }
    apply_parent_links(
        fs,
        device,
        request.old_parent,
        request.new_parent,
        parent_links,
    )?;
    fs.touch_inode_ctime_for_link_change(device, source.ino)?;
    fs.touch_inode_ctime_for_link_change(device, target.ino)?;
    Ok(RenameOutcome::default())
}

fn prepare_rename<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: RenameEntryRequest<'_>,
) -> Ext4Result<Option<ResolvedRename>> {
    if request.old_name.is_reserved() || request.new_name.is_reserved() {
        return Err(Ext4Error::invalid_input().with_operation("rename:reserved_name"));
    }
    let old_parent_inode = fs.get_inode_by_num(device, request.old_parent)?;
    let new_parent_inode = fs.get_inode_by_num(device, request.new_parent)?;
    if !old_parent_inode.is_dir() || !new_parent_inode.is_dir() {
        return Err(Ext4Error::not_dir());
    }
    let source = find_named_entry_in_parent(
        fs,
        device,
        request.old_parent,
        &old_parent_inode,
        request.old_name.as_bytes(),
    )?;
    let source_inode = fs.get_inode_by_num(device, source.ino)?;
    let destination = optional_entry(
        fs,
        device,
        request.new_parent,
        &new_parent_inode,
        request.new_name,
    )?;

    if request.options.no_replace() && destination.is_some() {
        return Err(Ext4Error::already_exists());
    }
    if destination.is_some_and(|entry| entry.ino == source.ino) {
        return Ok(None);
    }
    if request.old_parent == request.new_parent && request.old_name == request.new_name {
        return Ok(None);
    }

    if request.options.whiteout() {
        return Err(Ext4Error::unsupported().with_operation("rename:whiteout"));
    }

    let destination_inode = match destination {
        Some(entry) => Some(fs.get_inode_by_num(device, entry.ino)?),
        None => None,
    };
    Ok(Some(ResolvedRename {
        old_parent_inode,
        source,
        source_inode,
        new_parent_inode,
        destination,
        destination_inode,
    }))
}

fn flush_rename_metadata<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: RenameEntryRequest<'_>,
    resolved: ResolvedRename,
    counters_before: &[GroupCounters],
) -> Ext4Result<()> {
    let mut touched_inodes = BTreeSet::new();
    touched_inodes.insert(request.old_parent);
    touched_inodes.insert(request.new_parent);
    touched_inodes.insert(resolved.source.ino);
    if let Some(destination) = resolved.destination {
        touched_inodes.insert(destination.ino);
    }
    for inode in touched_inodes {
        fs.inodetable_cache.flush(device, inode)?;
    }
    fs.flush_changed_group_metadata(device, counters_before)?;
    fs.sync_superblock(device)
}

/// Renames two entries after both parent directories have been resolved.
pub(crate) fn rename_inode_at<B: BlockIo>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    request: RenameEntryRequest<'_>,
) -> Ext4Result<RenameOutcome> {
    let Some(resolved) = prepare_rename(fs, device, request)? else {
        return Ok(RenameOutcome::default());
    };
    let credits = if request.options.exchange() {
        RENAME_EXCHANGE_TRANSACTION_CREDITS
    } else {
        RENAME_TRANSACTION_CREDITS
    };
    let counters_before = fs.group_counter_snapshot();
    fs.with_metadata_transaction(device, credits, |fs, device| {
        let outcome = if request.options.exchange() {
            rename_exchange(fs, device, request, resolved)?
        } else {
            rename_replace(fs, device, request, resolved)?
        };
        flush_rename_metadata(fs, device, request, resolved, &counters_before)?;
        Ok(outcome)
    })
}

fn split_parent(path: &str) -> Ext4Result<(String, String)> {
    let split = path
        .rfind('/')
        .ok_or_else(|| Ext4Error::invalid_input().with_operation("rename:path"))?;
    let parent = if split == 0 {
        "/".to_string()
    } else {
        path[..split].to_string()
    };
    Ok((parent, path[split + 1..].to_string()))
}

/// Path-based rename operation with typed options.
pub fn rename<B: BlockIo>(
    device: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
    old_path: &str,
    new_path: &str,
    options: RenameOptions,
) -> Ext4Result<RenameOutcome> {
    let old_path = normalize_path(old_path);
    let new_path = normalize_path(new_path);
    if old_path == "/" || new_path == "/" {
        return Err(Ext4Error::invalid_input());
    }
    let (old_parent_path, old_name) = split_parent(&old_path)?;
    let (new_parent_path, new_name) = split_parent(&new_path)?;
    let (old_parent, _) =
        get_inode_with_num(fs, device, &old_parent_path)?.ok_or_else(Ext4Error::not_found)?;
    let (new_parent, _) =
        get_inode_with_num(fs, device, &new_parent_path)?.ok_or_else(Ext4Error::not_found)?;
    rename_inode_at(
        fs,
        device,
        RenameEntryRequest {
            old_parent,
            old_name: FileName::new(old_name.as_bytes())?,
            new_parent,
            new_name: FileName::new(new_name.as_bytes())?,
            options,
        },
    )
}
