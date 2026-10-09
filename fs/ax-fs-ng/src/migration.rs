//! Transactional migration of immutable boot resources.
//!
//! A migration copies resources from an initramfs (or another source context)
//! to a prepared filesystem.  Every destination is staged and validated
//! before any destination is published.  If publication fails, destinations
//! already published by this plan are restored from their backups.

use alloc::{boxed::Box, format, string::String, vec::Vec};

use axfs_ng_vfs::{NodeType, RenameOptions, VfsError, VfsResult};

use crate::{bundle, file::File, highlevel::FsContext};

/// Kinds of resources that may be copied by a [`MigrationPlan`].
///
/// The plan intentionally describes immutable boot inputs.  User data and
/// writable guest disks have explicit variants so callers cannot accidentally
/// treat them as immutable resources; adding either variant to a plan is
/// rejected with [`VfsError::InvalidInput`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    /// The target-side kernel symbol and source-location map.
    KernelMap,
    /// A host or guest kernel image.
    Kernel,
    /// A device-tree blob.
    DeviceTree,
    /// Firmware consumed during boot.
    Firmware,
    /// An initial ramdisk image.
    Initrd,
    /// Boot or virtual-machine configuration.
    BootConfig,
    /// A read-only guest image.
    GuestImage,
    /// Guest symbols or other read-only diagnostics metadata.
    GuestSymbols,
    /// An immutable resource not covered by a more specific category.
    Immutable,
    /// User-owned mutable data (not eligible for this transaction).
    UserData,
    /// A writable guest disk (not eligible for this transaction).
    WritableGuestDisk,
}

impl ResourceKind {
    const fn is_migratable(self) -> bool {
        !matches!(self, Self::UserData | Self::WritableGuestDisk)
    }
}

/// Compatibility name for callers that prefer the manifest terminology.
pub type MigrationResourceKind = ResourceKind;

/// How an entry handles an existing destination.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverwritePolicy {
    /// Replace the destination atomically after validation.
    #[default]
    Replace,
    /// Keep an existing destination and skip this entry.
    KeepExisting,
    /// Fail if a destination already exists.
    RequireAbsent,
}

/// Compatibility name for manifest loaders.
pub type MigrationOverwritePolicy = OverwritePolicy;

/// A callback that validates a staged resource.
pub type MigrationValidator = dyn Fn(&FsContext, &str) -> VfsResult<()>;

/// One source-to-destination migration item.
pub struct MigrationEntry {
    source: String,
    target: String,
    kind: ResourceKind,
    expected_size: Option<u64>,
    expected_hash: Option<u64>,
    overwrite: OverwritePolicy,
    validator: Option<Box<MigrationValidator>>,
}

impl core::fmt::Debug for MigrationEntry {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("MigrationEntry")
            .field("source", &self.source)
            .field("target", &self.target)
            .field("kind", &self.kind)
            .field("expected_size", &self.expected_size)
            .field("expected_hash", &self.expected_hash)
            .field("overwrite", &self.overwrite)
            .finish_non_exhaustive()
    }
}

impl MigrationEntry {
    /// Creates an entry.  Path and resource-kind validation is performed when
    /// the entry is added to a plan, allowing a builder-style API.
    pub fn new(kind: ResourceKind, source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            kind,
            expected_size: None,
            expected_hash: None,
            overwrite: OverwritePolicy::Replace,
            validator: None,
        }
    }

    /// Creates an entry with source and target first, for manifest parsers
    /// whose field order follows the on-disk schema.
    pub fn from_paths(
        source: impl Into<String>,
        target: impl Into<String>,
        kind: ResourceKind,
    ) -> Self {
        Self::new(kind, source, target)
    }

    /// Sets an expected byte length for the staged resource.
    pub const fn with_expected_size(mut self, size: u64) -> Self {
        self.expected_size = Some(size);
        self
    }

    /// Sets an expected FNV-1a 64-bit content hash for a regular file.
    pub const fn with_expected_hash(mut self, hash: u64) -> Self {
        self.expected_hash = Some(hash);
        self
    }

    /// Selects the destination overwrite policy.
    pub const fn with_overwrite(mut self, overwrite: OverwritePolicy) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// Installs an additional validator, called with the staged destination.
    pub fn with_validator<F>(mut self, validator: F) -> Self
    where
        F: Fn(&FsContext, &str) -> VfsResult<()> + 'static,
    {
        self.validator = Some(Box::new(validator));
        self
    }

    /// Returns the source path in the archive context.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the destination path in the target context.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the declared resource category.
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }
}

/// A collection of immutable resources to migrate as one transaction.
#[derive(Debug, Default)]
pub struct MigrationPlan {
    entries: Vec<MigrationEntry>,
}

/// Result of a successful migration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MigrationReport {
    /// Number of entries copied and published.
    pub migrated: usize,
    /// Number of entries skipped because their source was absent or the
    /// destination was kept by policy.
    pub skipped: usize,
}

struct Pending<'a> {
    entry: &'a MigrationEntry,
    stage: String,
    backup: String,
    had_target: bool,
    published: bool,
    skipped: bool,
}

impl MigrationPlan {
    /// Creates an empty migration plan.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates the default set of optional immutable boot resources.
    ///
    /// Entries are explicit and use identical source and target paths. Missing
    /// paths are skipped during [`Self::execute`], while present paths are
    /// copied and validated before a prepared root is published. Mutable guest
    /// disks, models, calibration data, and user data require an explicit
    /// manifest and identity policy and are intentionally absent here.
    pub fn boot_resources() -> VfsResult<Self> {
        const RESOURCES: &[(ResourceKind, &str)] = &[
            (ResourceKind::BootConfig, "/boot/config"),
            (ResourceKind::Kernel, "/boot/kernel"),
            (ResourceKind::DeviceTree, "/boot/dtb"),
            (ResourceKind::Firmware, "/boot/firmware"),
            (ResourceKind::Initrd, "/boot/initrd"),
        ];
        let mut plan = Self::new();
        for &(kind, path) in RESOURCES {
            plan.add(MigrationEntry::new(kind, path, path))?;
        }
        Ok(plan)
    }

    /// Adds an entry after checking its category and paths.
    pub fn add(&mut self, entry: MigrationEntry) -> VfsResult<()> {
        validate_entry_shape(&entry)?;
        if self
            .entries
            .iter()
            .any(|current| paths_overlap(&current.target, &entry.target))
        {
            return Err(VfsError::InvalidInput);
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Alias for [`Self::add`] suitable for manifest loaders.
    pub fn push(&mut self, entry: MigrationEntry) -> VfsResult<()> {
        self.add(entry)
    }

    /// Returns the number of entries in the plan.
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether this plan has no entries.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns an iterator over immutable plan entries.
    pub fn iter(&self) -> core::slice::Iter<'_, MigrationEntry> {
        self.entries.iter()
    }

    /// Copies and publishes every entry as one recoverable transaction.
    ///
    /// A missing source preserves an existing destination and is reported as
    /// skipped, matching [`bundle::install_directory`] semantics.  All
    /// staging and validation happens before the first publication.  A
    /// publication or flush error restores destinations already published by
    /// this call and removes remaining staging files.
    pub fn execute(&self, source: &FsContext, target: &FsContext) -> VfsResult<MigrationReport> {
        let mut pending = Vec::with_capacity(self.entries.len());

        // Recover interrupted per-entry transactions before staging.  This
        // also removes stale `.new` trees left by a power loss.
        for entry in &self.entries {
            let stage = format_path(&entry.target, ".new");
            let backup = format_path(&entry.target, ".old");
            if let Err(error) = bundle::recover(
                target,
                &entry.target,
                &stage,
                &backup,
                &validate_recovered_target,
                &bundle::flush,
            ) {
                cleanup_staging(target, &pending, Some(&stage));
                return Err(error);
            }
            let had_target = match bundle::exists(target, &entry.target) {
                Ok(value) => value,
                Err(error) => {
                    cleanup_staging(target, &pending, None);
                    return Err(error);
                }
            };
            if entry.overwrite == OverwritePolicy::RequireAbsent && had_target {
                cleanup_staging(target, &pending, None);
                return Err(VfsError::AlreadyExists);
            }
            if entry.overwrite == OverwritePolicy::KeepExisting && had_target {
                pending.push(Pending {
                    entry,
                    stage,
                    backup,
                    had_target,
                    published: false,
                    skipped: true,
                });
                continue;
            }
            let source_exists = match bundle::exists(source, &entry.source) {
                Ok(value) => value,
                Err(error) => {
                    cleanup_staging(target, &pending, None);
                    return Err(error);
                }
            };
            if !source_exists {
                pending.push(Pending {
                    entry,
                    stage,
                    backup,
                    had_target,
                    published: false,
                    skipped: true,
                });
                continue;
            }
            let parent = entry
                .target
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent);
            if let Err(error) = bundle::mkdir_parents(target, parent) {
                cleanup_staging(target, &pending, None);
                return Err(error);
            }
            if let Err(error) = bundle::copy_tree(source, &entry.source, target, &stage) {
                cleanup_staging(target, &pending, Some(&stage));
                return Err(error);
            }
            if let Err(error) = validate_entry_contents(entry, target, &stage) {
                cleanup_staging(target, &pending, Some(&stage));
                return Err(error);
            }
            pending.push(Pending {
                entry,
                stage,
                backup,
                had_target,
                published: false,
                skipped: false,
            });
        }

        // Make every staged file durable before changing any destination.
        if let Err(error) = bundle::flush(target) {
            cleanup_staging(target, &pending, None);
            return Err(error);
        }

        for index in 0..pending.len() {
            if pending[index].skipped {
                continue;
            }
            let (stage, backup, target_path, had_target) = {
                let item = &pending[index];
                (
                    item.stage.clone(),
                    item.backup.clone(),
                    item.entry.target.clone(),
                    item.had_target,
                )
            };
            let mut backed_up = false;
            let result = (|| {
                if had_target {
                    bundle::rename(target, &target_path, &backup, RenameOptions::NO_REPLACE)?;
                    backed_up = true;
                }
                if let Err(error) =
                    bundle::rename(target, &stage, &target_path, RenameOptions::NO_REPLACE)
                {
                    if backed_up {
                        let _ = bundle::rename(
                            target,
                            &backup,
                            &target_path,
                            RenameOptions::NO_REPLACE,
                        );
                    }
                    return Err(error);
                }
                // The destination has changed; mark it before the durability
                // barrier so rollback also handles a flush failure.
                pending[index].published = true;
                bundle::flush(target)
            })();
            if let Err(error) = result {
                rollback(target, &pending[..=index], &pending[index + 1..]);
                return Err(error);
            }
        }

        // The new package is now durable.  Backups are no longer needed.
        for item in &pending {
            if item.published {
                let _ = bundle::remove_tree(target, &item.backup);
            }
        }
        if let Err(error) = bundle::flush(target) {
            // Every publication was flushed before reaching this point.  A
            // cleanup barrier failure leaves a valid package and the next
            // invocation can retry removing any leftover backups.
            log::warn!("migration backup cleanup flush deferred: {error:?}");
        }
        Ok(MigrationReport {
            migrated: pending.iter().filter(|item| item.published).count(),
            skipped: pending.iter().filter(|item| item.skipped).count(),
        })
    }

    /// Compatibility spelling for callers that treat migration as a commit
    /// operation on a prepared root.
    pub fn migrate(&self, source: &FsContext, target: &FsContext) -> VfsResult<MigrationReport> {
        self.execute(source, target)
    }
}

fn validate_entry_shape(entry: &MigrationEntry) -> VfsResult<()> {
    if !entry.kind.is_migratable()
        || !valid_path(&entry.source)
        || !valid_path(&entry.target)
        || entry.source == "/"
        || entry.target == "/"
    {
        return Err(VfsError::InvalidInput);
    }
    Ok(())
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.ends_with('/')
        && !path.split('/').any(|part| matches!(part, "." | ".."))
}

fn format_path(path: &str, suffix: &str) -> String {
    let mut result = String::from(path);
    result.push_str(suffix);
    result
}

fn validate_entry_contents(
    entry: &MigrationEntry,
    context: &FsContext,
    path: &str,
) -> VfsResult<()> {
    let location = context.resolve_no_follow(path)?;
    if !matches!(
        location.node_type(),
        NodeType::RegularFile | NodeType::Directory
    ) {
        return Err(VfsError::InvalidData);
    }
    if let Some(expected_size) = entry.expected_size
        && content_size_and_hash(context, path)?.0 != expected_size
    {
        return Err(VfsError::InvalidData);
    }
    if let Some(expected_hash) = entry.expected_hash
        && content_size_and_hash(context, path)?.1 != expected_hash
    {
        return Err(VfsError::InvalidData);
    }
    if let Some(validator) = &entry.validator {
        validator(context, path)?;
    }
    Ok(())
}

fn validate_recovered_target(context: &FsContext, path: &str) -> VfsResult<()> {
    let location = context.resolve_no_follow(path)?;
    if matches!(
        location.node_type(),
        NodeType::RegularFile | NodeType::Directory
    ) {
        Ok(())
    } else {
        Err(VfsError::InvalidData)
    }
}

/// Computes the FNV-1a hash used by [`MigrationEntry::with_expected_hash`].
pub fn content_hash(context: &FsContext, path: &str) -> VfsResult<u64> {
    let file = File::open(context, path)?;
    let mut buffer = alloc::vec![0; crate::os::memory::PAGE_SIZE];
    let mut offset = 0;
    let mut hash = 0xcbf29ce484222325u64;
    loop {
        let count = file.read_at(&mut buffer[..], offset)?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        offset += count as u64;
    }
    Ok(hash)
}

fn content_size_and_hash(context: &FsContext, path: &str) -> VfsResult<(u64, u64)> {
    let location = context.resolve_no_follow(path)?;
    match location.node_type() {
        NodeType::RegularFile => Ok((location.len()?, content_hash(context, path)?)),
        NodeType::Directory => {
            let mut names = bundle::child_names(context, path)?;
            names.sort();
            let mut size = 0u64;
            let mut hash = 0xcbf29ce484222325u64;
            for name in names {
                for byte in name.as_bytes() {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                hash ^= 0xff;
                hash = hash.wrapping_mul(0x100000001b3);
                let (child_size, child_hash) =
                    content_size_and_hash(context, &format!("{path}/{name}"))?;
                size = size
                    .checked_add(child_size)
                    .ok_or(VfsError::ValueOverflow)?;
                for byte in child_hash.to_le_bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
            }
            Ok((size, hash))
        }
        _ => Err(VfsError::InvalidData),
    }
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn cleanup_staging(target: &FsContext, pending: &[Pending<'_>], extra: Option<&str>) {
    for item in pending {
        let _ = bundle::remove_tree(target, &item.stage);
    }
    if let Some(stage) = extra {
        let _ = bundle::remove_tree(target, stage);
    }
}

fn rollback(target: &FsContext, published: &[Pending<'_>], remaining: &[Pending<'_>]) {
    cleanup_staging(target, remaining, None);
    for item in published.iter().rev() {
        if !item.published {
            continue;
        }
        let _ = bundle::remove_tree(target, &item.entry.target);
        if item.had_target {
            let _ = bundle::rename(
                target,
                &item.backup,
                &item.entry.target,
                RenameOptions::NO_REPLACE,
            );
        }
    }
    let _ = bundle::flush(target);
}

#[cfg(test)]
mod tests {
    use axfs_ng_vfs::Mountpoint;

    use super::*;
    use crate::MemoryFs;

    fn context() -> FsContext {
        FsContext::new(Mountpoint::new_root(&MemoryFs::new()).root_location())
    }

    #[test]
    fn rejects_writable_resources_and_invalid_paths() {
        let mut plan = MigrationPlan::new();
        assert_eq!(
            plan.add(MigrationEntry::new(
                ResourceKind::WritableGuestDisk,
                "/disk",
                "/disk",
            )),
            Err(VfsError::InvalidInput)
        );
        assert_eq!(
            plan.add(MigrationEntry::new(
                ResourceKind::KernelMap,
                "/map/../bad",
                "/map",
            )),
            Err(VfsError::InvalidInput)
        );
    }

    #[test]
    fn stages_and_hashes_resources() {
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            let source = context();
            let target = context();
            crate::bundle::mkdir_parents(&source, "/symbols").unwrap();
            crate::bundle::mkdir_parents(&target, "/symbols").unwrap();
            source.write("/symbols/kernel.map", b"new").unwrap();
            target.write("/symbols/kernel.map", b"old").unwrap();
            let hash = {
                let mut plan = MigrationPlan::new();
                plan.add(MigrationEntry::new(
                    ResourceKind::KernelMap,
                    "/symbols/kernel.map",
                    "/symbols/kernel.map",
                ))
                .unwrap();
                content_hash(&source, "/symbols/kernel.map").unwrap()
            };
            let mut plan = MigrationPlan::new();
            plan.add(
                MigrationEntry::new(
                    ResourceKind::KernelMap,
                    "/symbols/kernel.map",
                    "/symbols/kernel.map",
                )
                .with_expected_hash(hash),
            )
            .unwrap();
            assert_eq!(plan.execute(&source, &target).unwrap().migrated, 1);
            assert_eq!(target.read("/symbols/kernel.map").unwrap(), b"new");
        });
    }

    #[test]
    fn validation_failure_keeps_the_installed_resource() {
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            let source = context();
            let target = context();
            crate::bundle::mkdir_parents(&source, "/symbols").unwrap();
            crate::bundle::mkdir_parents(&target, "/symbols").unwrap();
            source.write("/symbols/kernel.map", b"new").unwrap();
            target.write("/symbols/kernel.map", b"old").unwrap();
            let mut plan = MigrationPlan::new();
            plan.add(
                MigrationEntry::new(
                    ResourceKind::KernelMap,
                    "/symbols/kernel.map",
                    "/symbols/kernel.map",
                )
                .with_validator(|_, _| Err(VfsError::InvalidData)),
            )
            .unwrap();
            assert_eq!(plan.execute(&source, &target), Err(VfsError::InvalidData));
            assert_eq!(target.read("/symbols/kernel.map").unwrap(), b"old");
            assert!(!crate::bundle::exists(&target, "/symbols/kernel.map.new").unwrap());
        });
    }
}
