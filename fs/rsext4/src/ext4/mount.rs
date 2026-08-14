use super::{mkfs::read_superblock, *};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MountOptions {
    pub readonly: bool,
    pub replay_journal: bool,
    pub block_validity: bool,
}

impl MountOptions {
    pub const fn read_write() -> Self {
        Self {
            readonly: false,
            replay_journal: true,
            block_validity: true,
        }
    }

    pub const fn read_only_no_journal_replay() -> Self {
        Self {
            readonly: true,
            replay_journal: false,
            block_validity: true,
        }
    }

    pub const fn with_block_validity(mut self, enabled: bool) -> Self {
        self.block_validity = enabled;
        self
    }
}

fn validate_recovered_journal_mapping(
    expected_inode: InodeNumber,
    expected_blocks: &[AbsoluteBN],
    recovered_inode: InodeNumber,
    recovered_blocks: &[AbsoluteBN],
) -> Ext4Result<()> {
    if expected_inode != recovered_inode || expected_blocks != recovered_blocks {
        return Err(Ext4Error::corrupted().with_operation("journal:mapping_changed_during_replay"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalBootstrap {
    ExistingOnly,
    CreateDefaultForMkfs,
}

impl Ext4FileSystem {
    pub fn device_has_error_state<B: BlockIo>(block_dev: &mut Jbd2Dev<B>) -> Ext4Result<bool> {
        let superblock = read_superblock(block_dev).map_err(|_| Ext4Error::io())?;
        if superblock.s_magic != EXT4_SUPER_MAGIC {
            return Err(Ext4Error::invalid_magic());
        }
        superblock.verify_superblock()?;
        superblock.validate_geometry()?;
        Ok(superblock.s_state & Ext4Superblock::EXT4_ERROR_FS != 0)
    }

    /// Creates the root directory tree during bootstrap.
    fn create_root_dir<B: BlockIo>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        // The actual on-disk initialization lives in the dedicated directory
        // bootstrap helper.
        create_root_directory_entry(self, block_dev)
    }

    fn dirty_for_mount(superblock: &mut Ext4Superblock) {
        superblock.s_state &= !Ext4Superblock::EXT4_VALID_FS;
        superblock.s_mnt_count = superblock.s_mnt_count.saturating_add(1);
    }

    fn initialize_directory_hash_policy(superblock: &mut Ext4Superblock, read_only: bool) {
        let hash_policy =
            Ext4Superblock::EXT4_FLAGS_SIGNED_HASH | Ext4Superblock::EXT4_FLAGS_UNSIGNED_HASH;
        if !read_only
            && superblock.has_feature_compat(Ext4Superblock::EXT4_FEATURE_COMPAT_DIR_INDEX)
            && superblock.s_flags & hash_policy == 0
        {
            // Rust byte semantics are architecture-independent. Persist the signed policy used
            // by Linux on the reference architecture instead of leaving future lookup dependent
            // on the mounting compiler's plain-char signedness.
            superblock.s_flags |= Ext4Superblock::EXT4_FLAGS_SIGNED_HASH;
        }
    }

    fn inode_cache_size(superblock: &Ext4Superblock) -> usize {
        match superblock.s_inode_size {
            0 => GOOD_OLD_INODE_SIZE as usize,
            n => n as usize,
        }
    }

    fn reset_runtime_from_superblock<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        block_validity: bool,
    ) -> Ext4Result<()> {
        self.group_count = self.superblock.checked_block_groups_count()?;
        self.group_descs =
            Self::load_group_descriptors(block_dev, &self.superblock, self.group_count)?;
        self.dirty_group_descs = ::alloc::vec![false; self.group_descs.len()];
        self.system_zones = if block_validity {
            SystemZoneMap::from_layout(&self.superblock, &self.group_descs)?
        } else {
            SystemZoneMap::default()
        };
        self.block_allocator = BlockAllocator::new(&self.superblock);
        self.inode_allocator = InodeAllocator::new(&self.superblock);
        self.bitmap_cache = BitmapCache::create_default();
        self.inodetable_cache =
            InodeCache::new(INODE_CACHE_MAX, Self::inode_cache_size(&self.superblock));
        self.datablock_cache = DataBlockCache::new(DATABLOCK_CACHE_MAX, self.block_size());
        Ok(())
    }

    fn reload_after_journal_replay<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        read_only: bool,
        block_validity: bool,
    ) -> Ext4Result<()> {
        self.superblock = read_superblock(block_dev).map_err(|_| Ext4Error::io())?;
        self.superblock.verify_superblock()?;
        self.superblock.validate_geometry()?;
        self.superblock.check_features(read_only)?;
        Self::validate_journal_source(&self.superblock)?;
        Self::initialize_directory_hash_policy(&mut self.superblock, read_only);
        if !read_only {
            Self::dirty_for_mount(&mut self.superblock);
        }
        self.superblock_dirty = !read_only;
        self.reset_runtime_from_superblock(block_dev, block_validity)
    }

    fn check_mount_features<O: crate::runtime::Observer>(
        superblock: &Ext4Superblock,
        read_only: bool,
        observer: &mut O,
    ) -> Ext4Result<()> {
        use crate::runtime::{Event, FeatureEvent};

        let unsupported_incompat = superblock.unsupported_incompat_features(read_only);
        if unsupported_incompat != 0 {
            observer.event(Event::Feature(FeatureEvent::UnsupportedIncompat(
                unsupported_incompat,
            )));
        }

        let unsupported_ro_compat = superblock.unsupported_ro_compat_features();
        if unsupported_ro_compat != 0 {
            observer.event(Event::Feature(FeatureEvent::ReadOnlyCompat(
                unsupported_ro_compat,
            )));
        }

        superblock.check_features(read_only)
    }

    fn validate_journal_source(superblock: &Ext4Superblock) -> Ext4Result<()> {
        if !superblock.has_journal() {
            return Ok(());
        }

        match (
            superblock.s_journal_inum != 0,
            superblock.s_journal_dev != 0,
        ) {
            (true, false) => Ok(()),
            (false, true) => Err(Ext4Error::unsupported_capability(
                "block_io:external_journal",
            )),
            (true, true) => {
                Err(Ext4Error::invalid_input().with_operation("journal:ambiguous_source"))
            }
            (false, false) => Err(Ext4Error::corrupted().with_operation("journal:missing_source")),
        }
    }

    fn clear_recovery_state(&mut self) {
        self.superblock.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
        self.mark_superblock_dirty();
    }

    fn set_recovery_state(&mut self) {
        self.superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
        self.mark_superblock_dirty();
    }

    fn valid_lost_found_hint<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<bool> {
        let ino = self.superblock.s_lpf_ino;
        if ino == 0 {
            return Ok(false);
        }

        let inode = self.get_inode_by_num(block_dev, InodeNumber::new(ino)?)?;
        Ok(inode.i_mode != 0 && inode.is_dir())
    }

    fn journal_blocks<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        journal_inode_num: InodeNumber,
        journal_inode: &mut Ext4Inode,
    ) -> Ext4Result<Vec<AbsoluteBN>> {
        let journal_block_count = journal_inode.size().div_ceil(self.block_size() as u64);
        let journal_block_map =
            resolve_inode_blocks(self, block_dev, journal_inode_num, journal_inode)?;
        let mut journal_blocks = Vec::new();
        for logical in 0..journal_block_count {
            let logical = u32::try_from(logical).map_err(|_| Ext4Error::corrupted())?;
            let phys = journal_block_map
                .get(&logical)
                .copied()
                .ok_or_else(Ext4Error::corrupted)?;
            journal_blocks.push(phys);
        }
        Ok(journal_blocks)
    }

    fn protect_journal_blocks<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        journal_inode_num: InodeNumber,
    ) -> Ext4Result<Vec<AbsoluteBN>> {
        let mut journal_inode = self.get_inode_by_num(block_dev, journal_inode_num)?;
        let journal_blocks =
            self.journal_blocks(block_dev, journal_inode_num, &mut journal_inode)?;
        if !self.system_zones.is_empty() {
            self.system_zones = self.system_zones.with_owned_blocks(
                &self.superblock,
                journal_inode_num,
                &journal_blocks,
            )?;
        }
        Ok(journal_blocks)
    }

    pub(crate) fn set_block_validity<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        enabled: bool,
    ) -> Ext4Result<()> {
        if !enabled {
            self.system_zones = SystemZoneMap::default();
            return Ok(());
        }

        let mut system_zones = SystemZoneMap::from_layout(&self.superblock, &self.group_descs)?;
        if self.superblock.has_journal() {
            let journal_inode_num = InodeNumber::new(self.superblock.s_journal_inum)
                .map_err(|_| Ext4Error::corrupted().with_operation("journal:inode_number"))?;
            let mut journal_inode = self.get_inode_by_num(block_dev, journal_inode_num)?;
            let journal_blocks =
                self.journal_blocks(block_dev, journal_inode_num, &mut journal_inode)?;
            system_zones = system_zones.with_owned_blocks(
                &self.superblock,
                journal_inode_num,
                &journal_blocks,
            )?;
        }
        self.system_zones = system_zones;
        Ok(())
    }

    pub(crate) fn remount_read_only<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        if self.superblock.s_last_orphan != 0 {
            return Err(Ext4Error::busy().with_operation("remount:live_orphans"));
        }

        let previous_superblock = self.superblock;
        let previous_superblock_dirty = self.superblock_dirty;
        self.superblock.s_state = (self.superblock.s_state & Ext4Superblock::EXT4_ERROR_FS)
            | Ext4Superblock::EXT4_VALID_FS;
        self.mark_superblock_dirty();
        self.clear_recovery_state();
        if let Err(error) = self.sync_filesystem(block_dev) {
            self.superblock = previous_superblock;
            self.superblock_dirty = previous_superblock_dirty;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn remount_read_write<B: BlockIo, O: crate::runtime::Observer>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        observer: &mut O,
    ) -> Ext4Result<()> {
        if block_dev.device_is_read_only() {
            return Err(Ext4Error::read_only().with_operation("remount:device_read_only"));
        }
        Self::check_mount_features(&self.superblock, false, observer)?;
        if self
            .superblock
            .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER)
        {
            return Err(Ext4Error::busy().with_operation("remount:recovery_required"));
        }
        if self.superblock.s_last_orphan != 0 {
            return Err(Ext4Error::busy().with_operation("remount:live_orphans"));
        }
        if self.superblock.has_journal() && !block_dev.is_use_journal() {
            return Err(Ext4Error::unsupported().with_operation("remount:journal_disabled"));
        }

        let previous_superblock = self.superblock;
        let previous_superblock_dirty = self.superblock_dirty;
        Self::dirty_for_mount(&mut self.superblock);
        self.mark_superblock_dirty();
        if self.superblock.has_journal() {
            self.set_recovery_state();
        }
        if let Err(error) = self.sync_filesystem(block_dev) {
            self.superblock = previous_superblock;
            self.superblock_dirty = previous_superblock_dirty;
            return Err(error);
        }
        Ok(())
    }

    /// Mounts an ext4 filesystem from the given block device.
    pub fn mount<B: BlockIo>(block_dev: &mut Jbd2Dev<B>) -> Result<Self, Ext4Error> {
        Self::mount_with_options(block_dev, MountOptions::read_write())
    }

    pub fn mount_with_options<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        options: MountOptions,
    ) -> Result<Self, Ext4Error> {
        let mut observer = crate::runtime::NoopObserver;
        Self::mount_with_options_and_observer(block_dev, options, &mut observer)
    }

    pub fn mount_with_options_and_observer<B: BlockIo, O: crate::runtime::Observer>(
        block_dev: &mut Jbd2Dev<B>,
        options: MountOptions,
        observer: &mut O,
    ) -> Result<Self, Ext4Error> {
        let mut entropy = ();
        let mut delay = ();
        Self::mount_with_services(
            block_dev,
            options,
            observer,
            &mut entropy,
            &mut delay,
            crate::runtime::MmpIdentity::default(),
        )
    }

    pub(crate) fn mount_with_services<B, O, E, W>(
        block_dev: &mut Jbd2Dev<B>,
        options: MountOptions,
        observer: &mut O,
        entropy: &mut E,
        delay: &mut W,
        mmp_identity: crate::runtime::MmpIdentity,
    ) -> Result<Self, Ext4Error>
    where
        B: BlockIo,
        O: crate::runtime::Observer,
        E: crate::runtime::EntropySource,
        W: crate::runtime::Delay,
    {
        Self::mount_with_services_and_journal_bootstrap(
            block_dev,
            options,
            observer,
            entropy,
            delay,
            mmp_identity,
            JournalBootstrap::ExistingOnly,
        )
    }

    pub(super) fn mount_for_mkfs<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
    ) -> Result<Self, Ext4Error> {
        let mut observer = crate::runtime::NoopObserver;
        let mut entropy = ();
        let mut delay = ();
        Self::mount_with_services_and_journal_bootstrap(
            block_dev,
            MountOptions::read_write(),
            &mut observer,
            &mut entropy,
            &mut delay,
            crate::runtime::MmpIdentity::default(),
            JournalBootstrap::CreateDefaultForMkfs,
        )
    }

    fn mount_with_services_and_journal_bootstrap<B, O, E, W>(
        block_dev: &mut Jbd2Dev<B>,
        options: MountOptions,
        observer: &mut O,
        entropy: &mut E,
        delay: &mut W,
        mmp_identity: crate::runtime::MmpIdentity,
        journal_bootstrap: JournalBootstrap,
    ) -> Result<Self, Ext4Error>
    where
        B: BlockIo,
        O: crate::runtime::Observer,
        E: crate::runtime::EntropySource,
        W: crate::runtime::Delay,
    {
        use crate::runtime::{Event, MountEvent};

        observer.event(Event::Mount(MountEvent::Started));
        match Self::mount_inner(
            block_dev,
            options,
            observer,
            entropy,
            delay,
            mmp_identity,
            journal_bootstrap,
        ) {
            Ok(fs) => {
                observer.event(Event::Mount(MountEvent::Succeeded));
                Ok(fs)
            }
            Err(error) => {
                observer.event(Event::Mount(MountEvent::Failed));
                Err(error)
            }
        }
    }

    fn mount_inner<B, O, E, W>(
        block_dev: &mut Jbd2Dev<B>,
        options: MountOptions,
        observer: &mut O,
        entropy: &mut E,
        delay: &mut W,
        mmp_identity: crate::runtime::MmpIdentity,
        journal_bootstrap: JournalBootstrap,
    ) -> Result<Self, Ext4Error>
    where
        B: BlockIo,
        O: crate::runtime::Observer,
        E: crate::runtime::EntropySource,
        W: crate::runtime::Delay,
    {
        use crate::runtime::{Event, IntegrityEvent, JournalEvent, RecoveryEvent, RepairEvent};

        // Mount flow:
        // 1. read and verify the superblock,
        // 2. load only enough metadata to locate/replay the journal,
        // 3. reload metadata from the recovered home blocks,
        // 4. repair bootstrap directories if they are missing.
        let mut superblock = read_superblock(block_dev).map_err(|_| Ext4Error::io())?;

        if superblock.s_magic != EXT4_SUPER_MAGIC {
            return Err(Ext4Error::invalid_magic());
        }
        superblock.verify_superblock()?;
        superblock.validate_geometry()?;
        if superblock.blocks_count() > block_dev.total_blocks() {
            return Err(Ext4Error::bad_superblock().with_operation("superblock:device_capacity"));
        }
        Self::check_mount_features(&superblock, options.readonly, observer)?;
        Self::validate_journal_source(&superblock)?;
        Self::initialize_directory_hash_policy(&mut superblock, options.readonly);

        // Continue mounting even for an error-state filesystem so higher layers
        // can inspect or attempt repair.
        if superblock.s_state & Ext4Superblock::EXT4_ERROR_FS != 0 {
            observer.event(Event::Integrity(IntegrityEvent::CorruptionDetected));
        }

        let group_count = superblock.checked_block_groups_count()?;

        let group_descs = Self::load_group_descriptors(block_dev, &superblock, group_count)?;
        let system_zones = if options.block_validity {
            SystemZoneMap::from_layout(&superblock, &group_descs)?
        } else {
            SystemZoneMap::default()
        };

        let block_allocator = BlockAllocator::new(&superblock);
        let inode_allocator = InodeAllocator::new(&superblock);

        let bitmap_cache = BitmapCache::create_default();

        // NOTE: inode size is a filesystem property (superblock.s_inode_size), not a fixed constant.
        // Using a wrong inode size will make inode table offsets incorrect and may read zeroed inodes
        // (e.g. /dev becomes mode=0, then VFS mount fails with ENOTDIR).
        let inode_cache = InodeCache::new(INODE_CACHE_MAX, Self::inode_cache_size(&superblock));

        let filesystem_block_size = superblock.checked_block_size()? as usize;
        let datablock_cache = DataBlockCache::new(DATABLOCK_CACHE_MAX, filesystem_block_size);

        let mut fs = Self {
            superblock,
            superblock_dirty: false,
            dirty_group_descs: ::alloc::vec![false; group_descs.len()],
            group_descs,
            block_allocator,
            inode_allocator,
            bitmap_cache,
            root_inode: InodeNumber::new(2)?,
            inodetable_cache: inode_cache,
            datablock_cache,
            group_count,
            mounted: true,
            mmp: super::mmp::MmpState::Disabled,
            journal_sb_block_start: None,
            system_zones,
        };

        if !options.readonly {
            fs.mmp = super::mmp::MmpState::claim(block_dev, &fs.superblock, entropy, delay)?;
            // Mark the filesystem as not cleanly unmounted only after MMP has
            // established this writer's exclusive ownership.
            Self::dirty_for_mount(&mut fs.superblock);
            fs.superblock_dirty = true;
        }

        let mount_result = (|| -> Ext4Result<()> {
            let mut journal_mapping: Option<(InodeNumber, Vec<AbsoluteBN>)> = None;
            // A normal mount only accepts an existing valid journal. The private
            // mkfs bootstrap path creates the default journal before loading it.
            {
                let needs_recovery = fs
                    .superblock
                    .has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER);
                if needs_recovery {
                    observer.event(Event::Recovery(RecoveryEvent::Required));
                }

                if fs.superblock.has_journal() {
                    let journal_inode_num = InodeNumber::new(fs.superblock.s_journal_inum)
                        .map_err(|_| {
                            Ext4Error::corrupted().with_operation("journal:inode_number")
                        })?;
                    let mut journal_inode = fs.get_inode_by_num(block_dev, journal_inode_num)?;
                    let journal_exists = journal_inode.i_mode != 0;
                    if !journal_exists {
                        if needs_recovery
                            || journal_bootstrap != JournalBootstrap::CreateDefaultForMkfs
                        {
                            observer.event(Event::Recovery(RecoveryEvent::JournalMissing));
                            return Err(
                                Ext4Error::corrupted().with_operation("journal:missing_inode")
                            );
                        }
                        if journal_inode_num.raw() != JOURNAL_FILE_INODE as u32 {
                            return Err(Ext4Error::corrupted()
                                .with_operation("journal:missing_custom_inode"));
                        }
                        create_journal_entry(&mut fs, block_dev)?;
                        journal_inode = fs.get_inode_by_num(block_dev, journal_inode_num)?;
                    }
                    if !journal_inode.is_file()
                        || journal_inode.i_links_count == 0
                        || journal_inode.i_flags & Ext4Inode::EXT4_ENCRYPT_FL != 0
                    {
                        observer.event(Event::Recovery(RecoveryEvent::JournalMissing));
                        return Err(Ext4Error::corrupted().with_operation("journal:invalid_inode"));
                    }

                    let journal_blocks = fs.protect_journal_blocks(block_dev, journal_inode_num)?;
                    journal_mapping = Some((journal_inode_num, journal_blocks));
                }
                if needs_recovery && options.replay_journal && !fs.superblock.has_journal() {
                    observer.event(Event::Recovery(RecoveryEvent::JournalMissing));
                    return Err(Ext4Error::corrupted());
                }
                if (block_dev.is_use_journal() || (needs_recovery && options.replay_journal))
                    && fs.superblock.has_journal()
                {
                    // By this point the journal inode must exist, so resolve its
                    // first data block and hand the loaded journal superblock to
                    // `Jbd2Dev`.
                    let (expected_journal_ino, journal_blocks) = journal_mapping
                        .as_ref()
                        .ok_or_else(|| Ext4Error::corrupted().with_operation("journal:mapping"))?;
                    let journal_first_block = journal_blocks
                        .first()
                        .copied()
                        .ok_or_else(Ext4Error::corrupted)?;

                    fs.journal_sb_block_start = Some(journal_first_block);
                    let journal_data = fs
                        .datablock_cache
                        .get_or_load(block_dev, journal_first_block)?
                        .data
                        .clone();

                    let j_sb = JournalSuperBlock::decode_checked(&journal_data)?;

                    block_dev.set_journal_superblock_with_mapping(j_sb, journal_blocks.clone())?;

                    if needs_recovery && options.replay_journal {
                        observer.event(Event::Journal(JournalEvent::ReplayRequested));
                        // Replay before touching ordinary filesystem metadata.
                        // Until this completes, home blocks may be stale. A clean
                        // filesystem with journaling enabled still needs JBD2
                        // state initialized for future metadata writes, but it
                        // must not force replay without the ext4 recovery bit.
                        let original_journal_use = block_dev.is_use_journal();
                        if !original_journal_use {
                            block_dev.set_journal_use(true)?;
                        }
                        match block_dev.journal_replay_checked() {
                            ReplayStatus::Complete => {}
                            ReplayStatus::Incomplete(failure) => {
                                let cause = failure.cause();
                                observer.event(Event::Recovery(RecoveryEvent::ReplayFailed {
                                    phase: failure.phase(),
                                    cause,
                                    persistence_error: failure.persistence_error(),
                                }));
                                return Err(cause);
                            }
                        }
                        block_dev.set_journal_use(original_journal_use)?;

                        // Journal replay can update the superblock, group
                        // descriptors, bitmaps, inode table, and directory blocks.
                        // Drop all metadata read before replay and continue
                        // mounting from the recovered on-disk state.
                        fs.reload_after_journal_replay(
                            block_dev,
                            options.readonly,
                            options.block_validity,
                        )?;
                        Self::check_mount_features(&fs.superblock, options.readonly, observer)?;
                        let recovered_journal_ino = InodeNumber::new(fs.superblock.s_journal_inum)
                            .map_err(|_| {
                                Ext4Error::corrupted().with_operation("journal:inode_number")
                            })?;
                        let recovered_journal_blocks =
                            fs.protect_journal_blocks(block_dev, recovered_journal_ino)?;
                        validate_recovered_journal_mapping(
                            *expected_journal_ino,
                            journal_blocks,
                            recovered_journal_ino,
                            &recovered_journal_blocks,
                        )?;
                        fs.clear_recovery_state();
                    } else if !options.readonly && block_dev.is_use_journal() {
                        fs.set_recovery_state();
                    }
                }
                // If the filesystem was created without a journal (e.g. small images
                // where mkfs.ext4 omits it), disable journal_use so that metadata
                // writes bypass the journal path instead of hitting the
                // "system uninitialized" guard on every write.
                if !fs.superblock.has_journal() {
                    block_dev.set_journal_use(false)?;
                }
            }

            // Classic orphan-list recovery must observe metadata after JBD2 replay
            // and complete before ordinary namespace repair can allocate inodes or
            // blocks. Read-only mounts validate but never mutate the chain.
            if options.readonly {
                fs.validate_orphan_chain(block_dev)?;
            } else {
                fs.recover_orphans(block_dev)?;
            }

            // rootinode check !
            {
                let root_inode = fs.get_root(block_dev).map_err(|_| Ext4Error::io())?;
                if root_inode.i_mode == 0 || !root_inode.is_dir() {
                    if options.readonly {
                        return Err(Ext4Error::corrupted());
                    }

                    fs.create_root_dir(block_dev).map_err(|_| Ext4Error::io())?;
                    observer.event(Event::Repair(RepairEvent::RootRecreated));
                }
            }

            // Verify the recovery directory after the root directory is known good.
            {
                if !fs.valid_lost_found_hint(block_dev)? {
                    match get_file_inode(&mut fs, block_dev, "/lost+found") {
                        Ok(Some((ino, inode))) if inode.is_dir() => {
                            fs.superblock.s_lpf_ino = ino.raw();
                            fs.mark_superblock_dirty();
                            if !options.readonly {
                                fs.sync_superblock(block_dev)?;
                            }
                            observer.event(Event::Repair(RepairEvent::DirectoryIndexFallback));
                        }
                        Ok(Some((_ino, _inode))) => {
                            return Err(Ext4Error::corrupted());
                        }
                        Ok(None) => {
                            if !options.readonly {
                                create_lost_found_directory(&mut fs, block_dev)?;
                                observer.event(Event::Repair(RepairEvent::LostFoundRecreated));
                            }
                        }
                        Err(err) => {
                            return Err(err);
                        }
                    }
                }
            }

            // Emit a one-shot bitmap usage summary and verify bitmap checksums on
            // group 0 when metadata checksums are enabled.
            {
                let g0 = match fs.group_descs.first() {
                    Some(desc) => desc,
                    None => return Err(Ext4Error::bad_superblock()),
                };
                let inode_bitmap_blk = g0.inode_bitmap();
                let data_bitmap_blk = g0.block_bitmap();
                let inode_cache_key = CacheKey::new_inode(BGIndex::new(0));
                let data_cache_key = CacheKey::new_block(BGIndex::new(0));

                let inode_bitmap_data = fs
                    .bitmap_cache
                    .get_or_load(
                        block_dev,
                        inode_cache_key,
                        AbsoluteBN::new(inode_bitmap_blk),
                    )?
                    .clone();
                let blockbitmap_data = fs.bitmap_cache.get_or_load(
                    block_dev,
                    data_cache_key,
                    AbsoluteBN::new(data_bitmap_blk),
                )?;

                if ext4_superblock_has_metadata_csum(&fs.superblock) {
                    if !g0.is_inode_bitmap_uninit() {
                        let computed_inode =
                            ext4_inode_bitmap_csum32(&fs.superblock, &inode_bitmap_data.data);
                        let expected_inode = computed_inode;
                        if !g0.inode_bitmap_csum_matches(&fs.superblock, expected_inode) {
                            observer.event(Event::Integrity(IntegrityEvent::ChecksumMismatch));

                            return Err(Ext4Error::checksum());
                        }
                    }

                    if !g0.is_block_bitmap_uninit() {
                        let computed_block =
                            ext4_block_bitmap_csum32(&fs.superblock, &blockbitmap_data.data);
                        let expected_block = computed_block;
                        if !g0.block_bitmap_csum_matches(&fs.superblock, expected_block) {
                            observer.event(Event::Integrity(IntegrityEvent::ChecksumMismatch));

                            return Err(Ext4Error::checksum());
                        }
                    }
                }
            }

            // Flush metadata once at the end of mount so any replay state changes
            // or bootstrap repairs are persisted before normal operation begins.
            // The superblock is written with EXT4_VALID_FS cleared so a later mount
            // can distinguish an unclean shutdown from a real EXT4_ERROR_FS state.
            if !options.readonly {
                fs.mmp.refresh(
                    block_dev,
                    &fs.superblock,
                    mmp_identity,
                    core::time::Duration::ZERO,
                )?;
                fs.sync_filesystem_with_observer(block_dev, observer)?;
                block_dev.umount_commit()?;
                observer.event(Event::Journal(JournalEvent::Committed));
            }

            Ok(())
        })();

        match mount_result {
            Ok(()) => Ok(fs),
            Err(error) => {
                let superblock = fs.superblock;
                let cleanup = fs.mmp.release_clean(block_dev, &superblock);
                Err(crate::file::error_after_cleanup(error, cleanup))
            }
        }
    }

    /// Loads all block-group descriptors in on-disk order.
    fn load_group_descriptors<B: BlockIo>(
        block_dev: &mut Jbd2Dev<B>,
        superblock: &Ext4Superblock,
        group_count: u32,
    ) -> Result<Vec<Ext4GroupDesc>, Ext4Error> {
        let mut group_descs = Vec::new();
        let gdt_base = superblock.primary_gdt_byte_offset()?;
        let block_size_u64 = u64::from(superblock.checked_block_size()?);

        // Cache the currently loaded GDT block to avoid rereading the same
        // block for neighboring descriptors.
        let mut current_block: Option<AbsoluteBN> = None;

        let desc_size = superblock.get_desc_size() as usize;

        for group_id in 0..group_count {
            let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
            let block_num = AbsoluteBN::new(byte_offset / block_size_u64);
            let in_block = (byte_offset % block_size_u64) as usize;

            if current_block != Some(block_num) {
                block_dev
                    .read_block(block_num)
                    .map_err(|_| Ext4Error::io())?;
                current_block = Some(block_num);
            }

            let buffer = block_dev.buffer();
            let end = in_block + desc_size;
            if end > buffer.len() {
                return Err(Ext4Error::bad_superblock());
            }

            let raw_record = &buffer[in_block..end];
            let desc = Ext4GroupDesc::decode_checked(raw_record)?;
            desc.verify_checksum_in_bytes(superblock, group_id, raw_record)?;
            if group_id == 0 && (desc.is_block_bitmap_uninit() || desc.is_inode_bitmap_uninit()) {
                return Err(Ext4Error::corrupted().with_operation("group_descriptor:group0_uninit"));
            }
            group_descs.push(desc);
        }

        Ok(group_descs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_mapping_must_not_change_during_replay() {
        let journal_inode = InodeNumber::new(JOURNAL_FILE_INODE as u32).unwrap();
        let other_inode = InodeNumber::new(JOURNAL_FILE_INODE as u32 + 1).unwrap();
        let original = [AbsoluteBN::new(40), AbsoluteBN::new(41)];
        let moved = [AbsoluteBN::new(40), AbsoluteBN::new(42)];

        assert!(
            validate_recovered_journal_mapping(journal_inode, &original, other_inode, &original)
                .is_err()
        );
        assert!(
            validate_recovered_journal_mapping(journal_inode, &original, journal_inode, &moved)
                .is_err()
        );
        assert!(
            validate_recovered_journal_mapping(journal_inode, &original, journal_inode, &original,)
                .is_ok()
        );
    }

    #[test]
    fn writable_mount_persists_an_unambiguous_directory_hash_policy() {
        let mut superblock = Ext4Superblock {
            s_feature_compat: Ext4Superblock::EXT4_FEATURE_COMPAT_DIR_INDEX,
            s_flags: 0,
            ..Default::default()
        };

        Ext4FileSystem::initialize_directory_hash_policy(&mut superblock, true);
        assert_eq!(superblock.s_flags, 0);
        Ext4FileSystem::initialize_directory_hash_policy(&mut superblock, false);
        assert_eq!(superblock.s_flags, Ext4Superblock::EXT4_FLAGS_SIGNED_HASH);

        superblock.s_flags = Ext4Superblock::EXT4_FLAGS_UNSIGNED_HASH;
        Ext4FileSystem::initialize_directory_hash_policy(&mut superblock, false);
        assert_eq!(superblock.s_flags, Ext4Superblock::EXT4_FLAGS_UNSIGNED_HASH);
    }
}
