//! Exclusive recovery and growth before inode capabilities are published.

use super::*;

/// Unpublished filesystem owner; dropping it never writes a clean marker.
/// Recovery uses the initial range. Inode and detached I/O capabilities remain
/// unavailable until publication. Failed preparation makes publication invalid.
pub struct Ext4Session<D: BlockIo, S> {
    /// Recovered owner, kept private until all requested preparation succeeds.
    mount: Ext4<D, S>,
    /// First preparation failure; publication never clears this terminal state.
    failure: Option<Ext4Error>,
}

impl<D, E, O, W> Ext4Session<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Recovers and initializes exactly once using the current device range.
    /// Neither success nor failure publishes inode capabilities.
    pub fn new<C>(
        device: D,
        services: MountServices<C, E, O, W>,
        options: MountOptions,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        Self::new_selecting_options(device, services, |_| Ok(options))
    }

    /// Selects read-only replay for recorded errors before recovery starts.
    /// A failed recovery is never retried with the same cache/journal owner.
    pub fn mount_with_readonly_fallback<C>(
        device: D,
        services: MountServices<C, E, O, W>,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        Self::new_selecting_options(device, services, |device| {
            if Ext4FileSystem::device_has_error_state(device)? {
                Ok(MountOptions {
                    readonly: true,
                    replay_journal: true,
                    block_validity: true,
                })
            } else {
                Ok(MountOptions::read_write())
            }
        })
    }

    /// Selects policy before recovery, sharing one device and journal owner.
    pub(super) fn new_selecting_options<C, F>(
        device: D,
        services: MountServices<C, E, O, W>,
        select_options: F,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
        F: FnOnce(&mut Jbd2Dev<D>) -> Ext4Result<MountOptions>,
    {
        let MountServices {
            clock,
            mut entropy,
            mut observer,
            mut mmp_delay,
            mmp_identity,
        } = services;
        let mut device = Jbd2Dev::with_clock(0, device, clock, true);
        let options = select_options(&mut device)?;
        let filesystem = Ext4FileSystem::mount_with_services(
            &mut device,
            options,
            &mut observer,
            &mut entropy,
            &mut mmp_delay,
            mmp_identity,
        )?;
        Ok(Self {
            mount: Ext4 {
                filesystem,
                device,
                services: MountedServices::new(entropy, observer, mmp_delay, mmp_identity),
                options,
                shutdown_ticket: None,
                unmount_error: None,
                writes: write::InodeWriteOwners::default(),
            },
            failure: None,
        })
    }

    /// Returns usable capacity in bytes using metadata reads only.
    /// `None` means growth is unsupported and leaves this session publishable;
    /// other errors make preparation terminal. This does not enlarge the device.
    pub fn growth_capacity_if_supported(&mut self, target_bytes: u64) -> Ext4Result<Option<u64>> {
        self.ensure_ready()?;
        match self.checked_growth_capacity(target_bytes) {
            Ok(capacity) => Ok(Some(capacity)),
            Err(error) if matches!(error.kind(), Ext4ErrorKind::Unsupported) => Ok(None),
            Err(error) => {
                self.failure = Some(error);
                Err(error)
            }
        }
    }

    /// Validates the recovered geometry without changing preparation state.
    fn checked_growth_capacity(&mut self, target_bytes: u64) -> Ext4Result<u64> {
        if target_bytes == 0 {
            return Err(Ext4Error::invalid_input().with_operation("grow:zero_capacity"));
        }
        let capacity_bytes = self.mount.growth_capacity(target_bytes)?;
        // The lower growth API treats smaller targets as no-ops. Preparation
        // must still reject a target that cannot contain the recovered filesystem.
        if capacity_bytes > target_bytes {
            return Err(Ext4Error::invalid_input().with_operation("grow:capacity_below_current"));
        }
        Ok(capacity_bytes)
    }

    /// Publishes the sole mounted owner without repeating recovery or I/O.
    /// Failed preparation cannot be published as healthy.
    pub fn publish(self) -> Ext4Result<Ext4<D, MountedServices<E, O, W>>> {
        self.ensure_ready()?;
        Ok(self.mount)
    }

    /// Returns mount policy without publishing filesystem capabilities.
    pub const fn options(&self) -> MountOptions {
        self.mount.options
    }

    /// Returns recovered filesystem identity for root selection.
    pub fn uuid(&self) -> [u8; 16] {
        self.mount.filesystem.superblock.s_uuid
    }

    /// Returns recovered geometry and free-space accounting.
    pub fn statfs(&self) -> FileSystemStats {
        self.mount.statfs()
    }

    /// Grows within the supplied device bounds without changing its geometry.
    /// Used after independent partition growth or for a whole-device filesystem.
    /// Errors consume the unpublished session; no inode capabilities are exposed.
    pub fn grow_to_current_capacity(mut self) -> Ext4Result<Self> {
        self.ensure_ready()?;
        if self.mount.options.readonly {
            return Err(Ext4Error::read_only().with_operation("grow:readonly_mount"));
        }
        self.mount
            .filesystem
            .grow_before_publish(&mut self.mount.device, &mut self.mount.services.observer)?;
        Ok(self)
    }

    /// Rejects every later preparation step after the first planning failure.
    fn ensure_ready(&self) -> Ext4Result<()> {
        self.failure.map_or(Ok(()), Err)
    }
}
