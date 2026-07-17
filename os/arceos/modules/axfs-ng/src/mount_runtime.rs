//! Transactional root filesystem publication and remount orchestration.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

use ax_errno::AxError;
use axfs_ng_vfs::{Filesystem, Location, Mountpoint};
use spin::Once;

use crate::{
    BlockDevice, BlockRegion, FilesystemKind, fs,
    highlevel::{FsContext, install_root_context, replace_root_context},
    lifecycle::{
        FsFreezePermit, FsFreezeProgress, FsGeneration, FsRemountPermit, FsRuntime, FsRuntimeError,
        FsRuntimeSnapshot,
    },
    os::sync::SpinMutex,
};

/// Immutable information required to reconstruct the complete mount set.
///
/// The retained block service is deliberately not exposed: filesystem
/// handoff must close admission through the runtime controller lease before
/// any device ownership can move to a guest.
///
/// ```compile_fail
/// fn bypass_freeze(recipe: &ax_fs_ng::MountRecipe) {
///     let _device = recipe.device();
/// }
/// ```
#[derive(Clone)]
pub struct MountRecipe {
    device: Arc<dyn BlockDevice>,
    region: BlockRegion,
    filesystem: Option<FilesystemKind>,
    description: Arc<str>,
    additional_mounts: Vec<AdditionalMountRecipe>,
}

/// One successfully published non-root mount retained for handoff replay.
#[derive(Clone)]
pub(crate) struct AdditionalMountRecipe {
    device: Arc<dyn BlockDevice>,
    region: BlockRegion,
    filesystem: FilesystemKind,
    mount_path: Arc<str>,
    description: Arc<str>,
}

/// Failure while freezing, detaching, or reconstructing the root filesystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FsHandoffError {
    #[error("root filesystem runtime is not initialized")]
    NotInitialized,
    #[error(
        "filesystem freeze is waiting for {active_operations} active operations and \
         {open_handles} open handles"
    )]
    DrainPending {
        /// Filesystem operations that started before the freeze boundary.
        active_operations: usize,
        /// Externally visible handles retaining the frozen generation.
        open_handles: usize,
    },
    #[error(transparent)]
    Lifecycle(#[from] FsRuntimeError),
    #[error("filesystem operation failed: {0}")]
    Filesystem(AxError),
}

struct FilesystemService {
    lifecycle: FsRuntime,
    recipe: SpinMutex<MountRecipe>,
    mounted_root: SpinMutex<Option<Location>>,
}

struct UnpublishedMountTree {
    root: Option<Location>,
}

static FILESYSTEM_SERVICE: Once<FilesystemService> = Once::new();

impl MountRecipe {
    /// Describes how to rebuild one root filesystem.
    pub fn new(
        device: Arc<dyn BlockDevice>,
        region: BlockRegion,
        filesystem: Option<FilesystemKind>,
        description: impl Into<String>,
    ) -> Self {
        let description: String = description.into();
        Self {
            device,
            region,
            filesystem,
            description: Arc::from(description),
            additional_mounts: Vec::new(),
        }
    }

    /// Returns the selected block region.
    pub const fn region(&self) -> BlockRegion {
        self.region
    }

    /// Returns the explicit filesystem kind, if detection selected one.
    pub const fn filesystem(&self) -> Option<FilesystemKind> {
        self.filesystem
    }

    /// Returns the stable root-device description used in diagnostics.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the number of filesystems reconstructed by this recipe.
    pub fn mounted_filesystem_count(&self) -> usize {
        self.additional_mounts.len() + 1
    }

    fn record_additional_mount(&mut self, recipe: AdditionalMountRecipe) {
        self.additional_mounts.push(recipe);
    }

    fn replay_mount_set<Root, Error>(
        &self,
        mount_root: impl FnOnce(&Self) -> Result<Root, Error>,
        mut mount_additional: impl FnMut(&Root, &AdditionalMountRecipe) -> Result<(), Error>,
    ) -> Result<Root, Error> {
        let root = mount_root(self)?;
        for recipe in &self.additional_mounts {
            mount_additional(&root, recipe)?;
        }
        Ok(root)
    }

    fn mount_tree(&self) -> Result<Location, AxError> {
        let tree = self.replay_mount_set(
            |recipe| recipe.mount().map(UnpublishedMountTree::new),
            |tree, recipe| recipe.mount_at(tree.root()),
        )?;
        Ok(tree.into_root())
    }

    fn flush_devices(&self) -> Result<(), AxError> {
        self.device.flush()?;
        for (index, recipe) in self.additional_mounts.iter().enumerate() {
            let already_flushed = Arc::ptr_eq(&self.device, &recipe.device)
                || self.additional_mounts[..index]
                    .iter()
                    .any(|previous| Arc::ptr_eq(&previous.device, &recipe.device));
            if !already_flushed {
                recipe.device.flush()?;
            }
        }
        Ok(())
    }

    fn mount(&self) -> Result<Filesystem, AxError> {
        match self.filesystem {
            Some(kind) => fs::new_from_device_with_kind(self.device.clone(), self.region, kind),
            None => fs::new_from_device(self.device.clone(), self.region),
        }
    }
}

impl AdditionalMountRecipe {
    pub(crate) fn new(
        device: Arc<dyn BlockDevice>,
        region: BlockRegion,
        filesystem: FilesystemKind,
        mount_path: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let mount_path: String = mount_path.into();
        let description: String = description.into();
        Self {
            device,
            region,
            filesystem,
            mount_path: Arc::from(mount_path),
            description: Arc::from(description),
        }
    }

    fn mount_at(&self, root: &Location) -> Result<(), AxError> {
        info!(
            "  remounting partition {} at {}",
            self.description, self.mount_path
        );
        let filesystem =
            fs::new_from_device_with_kind(self.device.clone(), self.region, self.filesystem)?;
        let mountpoint = crate::root::ensure_mountpoint_dir_result(root, &self.mount_path)?;
        mountpoint.mount(&filesystem)?;
        Ok(())
    }
}

impl fmt::Debug for AdditionalMountRecipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdditionalMountRecipe")
            .field("device", &self.device.name())
            .field("region", &self.region)
            .field("filesystem", &self.filesystem)
            .field("mount_path", &self.mount_path)
            .field("description", &self.description)
            .finish()
    }
}

impl UnpublishedMountTree {
    fn new(filesystem: Filesystem) -> Self {
        let mountpoint = Mountpoint::new_root(&filesystem);
        Self {
            root: Some(mountpoint.root_location()),
        }
    }

    fn root(&self) -> &Location {
        self.root
            .as_ref()
            .expect("unpublished mount tree must own its root until publication")
    }

    fn into_root(mut self) -> Location {
        self.root
            .take()
            .expect("unpublished mount tree must own its root until publication")
    }
}

impl Drop for UnpublishedMountTree {
    fn drop(&mut self) {
        let Some(root) = self.root.take() else {
            return;
        };
        if let Err(error) = root.unmount_all() {
            warn!("failed to clean up unpublished remount tree: {error:?}");
        }
    }
}

impl fmt::Debug for MountRecipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountRecipe")
            .field("device", &self.device.name())
            .field("region", &self.region)
            .field("filesystem", &self.filesystem)
            .field("description", &self.description)
            .field("additional_mounts", &self.additional_mounts)
            .finish()
    }
}

pub(crate) fn install_initial_mount(
    filesystem: Filesystem,
    recipe: MountRecipe,
) -> Result<Location, AxError> {
    if FILESYSTEM_SERVICE.get().is_some() {
        return Err(AxError::AlreadyExists);
    }
    let mountpoint = Mountpoint::new_root(&filesystem);
    let root = mountpoint.root_location();
    let lifecycle = FsRuntime::new_mounted();
    let generation = lifecycle.snapshot().generation;
    install_root_context(FsContext::new_managed(
        root.clone(),
        lifecycle.clone(),
        generation,
    ))?;
    FILESYSTEM_SERVICE.call_once(|| FilesystemService {
        lifecycle,
        recipe: SpinMutex::new(recipe),
        mounted_root: SpinMutex::new(Some(root.clone())),
    });
    Ok(root)
}

pub(crate) fn record_initial_additional_mount(recipe: AdditionalMountRecipe) {
    FILESYSTEM_SERVICE
        .get()
        .expect("root service must exist before mounting additional filesystems")
        .recipe
        .lock()
        .record_additional_mount(recipe);
}

/// Returns the current lifecycle snapshot after initial root publication.
pub fn filesystem_runtime_snapshot() -> Option<FsRuntimeSnapshot> {
    FILESYSTEM_SERVICE
        .get()
        .map(|service| service.lifecycle.snapshot())
}

/// Returns the complete root and additional-mount recipe retained for remount.
pub fn mount_recipe() -> Option<MountRecipe> {
    FILESYSTEM_SERVICE
        .get()
        .map(|service| service.recipe.lock().clone())
}

/// Stops new opens and operations for the current mounted generation.
pub fn begin_filesystem_freeze() -> Result<FsFreezePermit, FsHandoffError> {
    let service = service()?;
    let generation = service.lifecycle.snapshot().generation;
    service
        .lifecycle
        .begin_freeze(generation)
        .map_err(Into::into)
}

/// Cancels a handoff before the root was detached.
pub fn cancel_filesystem_freeze(permit: &FsFreezePermit) -> Result<(), FsHandoffError> {
    service()?
        .lifecycle
        .cancel_freeze(permit)
        .map_err(Into::into)
}

/// Returns non-blocking progress for a filesystem freeze transaction.
///
/// Callers should reschedule and retry after the reported generation leases
/// are released. This function never waits or performs filesystem I/O.
///
/// # Errors
///
/// Returns [`FsHandoffError::NotInitialized`] before initial root publication,
/// or [`FsHandoffError::Lifecycle`] if `permit` no longer identifies the
/// active freeze transaction.
pub fn poll_filesystem_freeze(permit: &FsFreezePermit) -> Result<FsFreezeProgress, FsHandoffError> {
    service()?
        .lifecycle
        .freeze_progress(permit)
        .map_err(Into::into)
}

/// Synchronizes and unmounts the root after all generation leases drain.
pub fn detach_filesystem(permit: &FsFreezePermit) -> Result<(), FsHandoffError> {
    let service = service()?;
    let recipe = service.recipe.lock().clone();
    let root = service
        .mounted_root
        .lock()
        .clone()
        .ok_or(FsHandoffError::Lifecycle(FsRuntimeError::InvalidTransition))?;
    run_detach_transaction(
        &service.lifecycle,
        permit,
        || {
            crate::shutdown_filesystems()?;
            recipe.flush_devices()
        },
        || root.unmount_all(),
        || recipe.flush_devices(),
        || {
            service
                .mounted_root
                .lock()
                .take()
                .map(drop)
                .ok_or(AxError::BadState)
        },
    )
}

/// Starts one attempt to reconstruct the detached root.
pub fn begin_filesystem_remount() -> Result<FsRemountPermit, FsHandoffError> {
    service()?.lifecycle.begin_remount().map_err(Into::into)
}

/// Reconstructs and atomically publishes the root for `permit`.
///
/// A mount failure transitions the lifecycle to `Failed`; callers may start a
/// new remount attempt after the block runtime has recovered the controller.
pub fn remount_filesystem(permit: FsRemountPermit) -> Result<FsGeneration, FsHandoffError> {
    let service = service()?;
    let recipe = service.recipe.lock().clone();
    let root = match recipe.mount_tree() {
        Ok(root) => root,
        Err(error) => {
            service.lifecycle.fail_remount(permit)?;
            return Err(FsHandoffError::Filesystem(error));
        }
    };
    run_remount_publication(&service.lifecycle, permit, |generation| {
        replace_root_context(FsContext::new_managed(
            root.clone(),
            service.lifecycle.clone(),
            generation,
        ));
        *service.mounted_root.lock() = Some(root);
        Ok(())
    })
}

/// Marks a remount attempt failed before filesystem construction begins.
pub fn fail_filesystem_remount(permit: FsRemountPermit) -> Result<(), FsHandoffError> {
    service()?
        .lifecycle
        .fail_remount(permit)
        .map_err(Into::into)
}

fn run_detach_transaction(
    lifecycle: &FsRuntime,
    permit: &FsFreezePermit,
    prepare: impl FnOnce() -> Result<(), AxError>,
    unmount: impl FnOnce() -> Result<(), AxError>,
    make_durable: impl FnOnce() -> Result<(), AxError>,
    unpublish: impl FnOnce() -> Result<(), AxError>,
) -> Result<(), FsHandoffError> {
    require_freeze_drained(lifecycle, permit)?;

    // Synchronization and the first device flush are non-destructive. Keeping
    // the runtime in `Freezing` lets the caller retry or cancel safely.
    prepare().map_err(FsHandoffError::Filesystem)?;

    if let Err(error) = unmount() {
        lifecycle.fail_detach(permit)?;
        return Err(FsHandoffError::Filesystem(error));
    }
    if let Err(error) = make_durable() {
        lifecycle.fail_detach(permit)?;
        return Err(FsHandoffError::Filesystem(error));
    }
    if let Err(error) = unpublish() {
        lifecycle.fail_detach(permit)?;
        return Err(FsHandoffError::Filesystem(error));
    }

    // Root ownership is no longer published before observers can see the
    // terminal `Detached` state.
    lifecycle.finish_detach(permit).map_err(Into::into)
}

fn require_freeze_drained(
    lifecycle: &FsRuntime,
    permit: &FsFreezePermit,
) -> Result<(), FsHandoffError> {
    match lifecycle.freeze_progress(permit)? {
        FsFreezeProgress::Drained => Ok(()),
        FsFreezeProgress::Pending {
            active_operations,
            open_handles,
        } => Err(FsHandoffError::DrainPending {
            active_operations,
            open_handles,
        }),
    }
}

fn run_remount_publication(
    lifecycle: &FsRuntime,
    permit: FsRemountPermit,
    publish: impl FnOnce(FsGeneration) -> Result<(), AxError>,
) -> Result<FsGeneration, FsHandoffError> {
    lifecycle.validate_remount_publication(&permit)?;
    let generation = permit.next_generation();
    if let Err(error) = publish(generation) {
        lifecycle.fail_remount(permit)?;
        return Err(FsHandoffError::Filesystem(error));
    }

    // Successful operations may observe `Mounted` only after the matching
    // root context and mounted-root owner have both been installed.
    lifecycle.finish_remount(permit).map_err(Into::into)
}

fn service() -> Result<&'static FilesystemService, FsHandoffError> {
    FILESYSTEM_SERVICE
        .get()
        .ok_or(FsHandoffError::NotInitialized)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{cell::RefCell, vec::Vec};

    use super::*;
    use crate::{BlockDeviceMetadata, FsRuntimeState};

    struct NoopBlockDevice;

    impl BlockDevice for NoopBlockDevice {
        fn name(&self) -> &str {
            "noop"
        }

        fn metadata(&self) -> BlockDeviceMetadata {
            BlockDeviceMetadata::new(16, 512).unwrap()
        }

        fn read_blocks(&self, _start_block: u64, _buffer: &mut [u8]) -> ax_errno::AxResult {
            Ok(())
        }

        fn write_blocks(&self, _start_block: u64, _buffer: &[u8]) -> ax_errno::AxResult {
            Ok(())
        }

        fn flush(&self) -> ax_errno::AxResult {
            Ok(())
        }
    }

    #[test]
    fn detach_unpublishes_root_before_publishing_detached_state() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();
        let order = RefCell::new(Vec::new());

        run_detach_transaction(
            &runtime,
            &freeze,
            || {
                order.borrow_mut().push("prepare");
                Ok(())
            },
            || {
                order.borrow_mut().push("unmount");
                Ok(())
            },
            || {
                order.borrow_mut().push("durable");
                Ok(())
            },
            || {
                assert_eq!(runtime.snapshot().state, FsRuntimeState::Freezing);
                order.borrow_mut().push("unpublish");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            order.into_inner(),
            ["prepare", "unmount", "durable", "unpublish"]
        );
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Detached);
    }

    #[test]
    fn failure_after_unmount_is_fail_closed_and_skips_publication() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();
        let unpublished = RefCell::new(false);

        let result = run_detach_transaction(
            &runtime,
            &freeze,
            || Ok(()),
            || Err(AxError::Io),
            || Ok(()),
            || {
                *unpublished.borrow_mut() = true;
                Ok(())
            },
        );

        assert_eq!(result, Err(FsHandoffError::Filesystem(AxError::Io)));
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Failed);
        assert!(!unpublished.into_inner());
    }

    #[test]
    fn preparation_failure_remains_freezing_and_can_be_cancelled() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();

        let result = run_detach_transaction(
            &runtime,
            &freeze,
            || Err(AxError::Io),
            || panic!("unmount must not run after synchronization failure"),
            || panic!("final flush must not run after synchronization failure"),
            || panic!("root must remain published after synchronization failure"),
        );

        assert_eq!(result, Err(FsHandoffError::Filesystem(AxError::Io)));
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Freezing);
        runtime.cancel_freeze(&freeze).unwrap();
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Mounted);
    }

    #[test]
    fn detach_reports_typed_drain_progress_before_running_io() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let _operation = runtime.begin_operation(generation).unwrap();
        let _handle = runtime.open_handle(generation).unwrap();
        let freeze = runtime.begin_freeze(generation).unwrap();

        let result = run_detach_transaction(
            &runtime,
            &freeze,
            || panic!("prepare must not run while generation leases remain"),
            || panic!("unmount must not run while generation leases remain"),
            || panic!("flush must not run while generation leases remain"),
            || panic!("unpublish must not run while generation leases remain"),
        );

        assert_eq!(
            result,
            Err(FsHandoffError::DrainPending {
                active_operations: 1,
                open_handles: 1,
            })
        );
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Freezing);
    }

    #[test]
    fn retained_mount_set_replays_additional_mounts_after_detach() {
        let device: Arc<dyn BlockDevice> = Arc::new(NoopBlockDevice);
        let mut recipe = MountRecipe::new(
            device.clone(),
            BlockRegion::new(0, 4),
            Some(FilesystemKind::Ext4),
            "root",
        );
        recipe.record_additional_mount(AdditionalMountRecipe::new(
            device.clone(),
            BlockRegion::new(4, 4),
            FilesystemKind::Fat,
            "/boot",
            "boot",
        ));
        recipe.record_additional_mount(AdditionalMountRecipe::new(
            device,
            BlockRegion::new(8, 4),
            FilesystemKind::Ext4,
            "/userdata",
            "userdata",
        ));
        assert_eq!(recipe.mounted_filesystem_count(), 3);
        let mounted_paths = RefCell::new(Vec::new());

        let replay = || {
            recipe
                .replay_mount_set(
                    |_| {
                        mounted_paths.borrow_mut().push(String::from("/"));
                        Ok::<_, AxError>(())
                    },
                    |_, additional| {
                        mounted_paths
                            .borrow_mut()
                            .push(String::from(additional.mount_path.as_ref()));
                        Ok(())
                    },
                )
                .unwrap();
        };

        replay();
        assert_eq!(
            mounted_paths.borrow().as_slice(),
            [
                String::from("/"),
                String::from("/boot"),
                String::from("/userdata")
            ]
        );
        mounted_paths.borrow_mut().clear();

        replay();
        assert_eq!(
            mounted_paths.borrow().as_slice(),
            [
                String::from("/"),
                String::from("/boot"),
                String::from("/userdata")
            ]
        );
    }

    #[test]
    fn remount_root_is_published_before_generation_becomes_mounted() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();
        runtime.finish_detach(&freeze).unwrap();
        let remount = runtime.begin_remount().unwrap();
        let expected = remount.next_generation();

        let published = run_remount_publication(&runtime, remount, |generation| {
            assert_eq!(generation, expected);
            assert_eq!(runtime.snapshot().state, FsRuntimeState::Remounting);
            Ok(())
        })
        .unwrap();

        assert_eq!(published, expected);
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Mounted);
    }

    #[test]
    fn failed_root_publication_burns_its_generation() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();
        runtime.finish_detach(&freeze).unwrap();
        let remount = runtime.begin_remount().unwrap();
        let failed_generation = remount.next_generation();

        assert_eq!(
            run_remount_publication(&runtime, remount, |_| Err(AxError::Io)),
            Err(FsHandoffError::Filesystem(AxError::Io))
        );
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Failed);

        let retry = runtime.begin_remount().unwrap();
        assert!(retry.next_generation().get() > failed_generation.get());
    }
}
