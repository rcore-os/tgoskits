use super::{mkfs::read_superblock, *};

impl Ext4FileSystem {
    /// Creates the root directory tree during bootstrap.
    fn create_root_dir<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> Ext4Result<()> {
        // The actual on-disk initialization lives in the dedicated directory
        // bootstrap helper.
        create_root_directory_entry(self, block_dev)
    }

    /// Mounts an ext4 filesystem from the given block device.
    pub fn mount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> Result<Self, Ext4Error> {
        debug!("Start mounting Ext4 filesystem...");

        // Mount flow:
        // 1. read and verify the superblock,
        // 2. load group descriptors and allocator state,
        // 3. repair bootstrap directories if they are missing,
        // 4. initialize journal replay state when journaling is enabled.
        let superblock = read_superblock(block_dev).map_err(|_| Ext4Error::io())?;

        if superblock.s_magic != EXT4_SUPER_MAGIC {
            error!(
                "Invalid magic: {:#x}, expected: {:#x}",
                superblock.s_magic, EXT4_SUPER_MAGIC
            );
            return Err(Ext4Error::invalid_magic());
        }
        debug!("Superblock magic verified");
        superblock.verify_superblock()?;

        // Continue mounting even for an error-state filesystem so higher layers
        // can inspect or attempt repair.
        if superblock.s_state == Ext4Superblock::EXT4_ERROR_FS {
            warn!("Filesystem is in error state");
        }

        let group_count = superblock.block_groups_count();
        debug!("Block group count: {group_count}");

        let group_descs = Self::load_group_descriptors(block_dev, &superblock, group_count)?;
        debug!("Loaded {} group descriptors", group_descs.len());

        let block_allocator = BlockAllocator::new(&superblock);
        let inode_allocator = InodeAllocator::new(&superblock);
        debug!("Allocators initialized");

        let bitmap_cache = BitmapCache::create_default();
        debug!("Bitmap cache initialized (lazy loading)");

        // NOTE: inode size is a filesystem property (superblock.s_inode_size), not a fixed constant.
        // Using a wrong inode size will make inode table offsets incorrect and may read zeroed inodes
        // (e.g. /dev becomes mode=0, then VFS mount fails with ENOTDIR).
        let inode_size = match superblock.s_inode_size {
            0 => DEFAULT_INODE_SIZE as usize,
            n => n as usize,
        };
        let inode_cache = InodeCache::new(INODE_CACHE_MAX, inode_size);
        debug!("Inode cache initialized");

        let datablock_cache = DataBlockCache::new(DATABLOCK_CACHE_MAX, BLOCK_SIZE);
        debug!("Data block cache initialized");

        let mut fs = Self {
            superblock,
            group_descs,
            block_allocator,
            inode_allocator,
            bitmap_cache,
            root_inode: InodeNumber::new(2)?,
            inodetable_cahce: inode_cache,
            datablock_cache,
            group_count,
            mounted: true,
            journal_sb_block_start: None,
        };
        // Dump the core topology once so later failures have useful context in
        // the logs.
        debug_super_and_desc(&fs.superblock, &fs);

        // rootinode check !
        debug!("Checking root directory...");
        {
            let root_inode = fs.get_root(block_dev).map_err(|_| Ext4Error::io())?;
            if root_inode.i_mode == 0 || !root_inode.is_dir() {
                warn!(
                    "Root inode is uninitialized or not a directory, creating root and \
                     lost+found... i_mode: {}, is_dir: {}",
                    root_inode.i_mode,
                    root_inode.is_dir()
                );
                fs.create_root_dir(block_dev).map_err(|_| Ext4Error::io())?;
            }
        }

        // Verify the recovery directory after the root directory is known good.
        debug!("Checking lost+found directory...");
        {
            // Trust the superblock hint when present, but still validate via a
            // path lookup so stale metadata does not silently pass.
            if fs.superblock.s_lpf_ino != 0 {
                let ino = fs.superblock.s_lpf_ino;
                debug!("Lost+found inode recorded in superblock: {ino}");
            } else {
                debug!("s_lpf_ino is 0, lost+found inode hint missing in superblock");
            }

            match find_file(&mut fs, block_dev, "/lost+found") {
                Ok(_inode) => {
                    info!("/lost+found exists (path resolution)");
                }
                Err(err) if err.code == Errno::ENOENT => {
                    info!("/lost+found not found by path scan;will create!");
                    if create_lost_found_directory(&mut fs, block_dev).is_err() {
                        warn!("/lost+found missing and create failed");
                    }
                }
                Err(err) => return Err(err),
            }
        }

        // Journal bootstrap has two stages: ensure the journal inode exists,
        // then load its superblock and enable replay on the device wrapper.
        {
            if fs.superblock.has_journal() {
                let mut jouranl_exist: bool = true;
                fs.modify_inode(
                    block_dev,
                    InodeNumber::new(JOURNAL_FILE_INODE as u32)?,
                    |ji| {
                        jouranl_exist = ji.i_mode != 0;
                    },
                )
                .expect("file system error panic!");

                if fs
                    .superblock
                    .has_feature_compat(Ext4Superblock::EXT4_FEATURE_COMPAT_HAS_JOURNAL)
                    && !jouranl_exist
                {
                    create_journal_entry(&mut fs, block_dev).expect("create journal entry failed");
                }
            }
            if block_dev.is_use_journal() && fs.superblock.has_journal() {
                // By this point the journal inode must exist, so resolve its
                // first data block and hand the loaded journal superblock to
                // `Jbd2Dev`.
                let mut j_inode = fs
                    .get_inode_by_num(block_dev, InodeNumber::new(JOURNAL_FILE_INODE as u32)?)
                    .expect("load journal inode failed");

                let journal_first_block = resolve_inode_block(block_dev, &mut j_inode, 0)
                    .and_then(|opt| opt.ok_or(Ext4Error::corrupted()))
                    .expect("resolve journal first block failed");

                fs.journal_sb_block_start = Some(journal_first_block);
                let journal_data = fs
                    .datablock_cache
                    .get_or_load(block_dev, journal_first_block)
                    .expect("load journal superblock block failed")
                    .data
                    .clone();

                let j_sb = JournalSuperBllockS::from_disk_bytes(&journal_data);

                block_dev.set_journal_superblock(j_sb, fs.journal_sb_block_start.unwrap());

                // Replay after reading the filesystem metadata. Superblock and
                // descriptor writes are already forced to media to avoid stale
                // reads during fast recovery.
                block_dev.journal_replay();
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
                )
                .expect("Blcok Read Failed!")
                .clone();
            let blockbitmap_data = fs
                .bitmap_cache
                .get_or_load(block_dev, data_cache_key, AbsoluteBN::new(data_bitmap_blk))
                .expect("Blcok Read Failed!");

            if ext4_superblock_has_metadata_csum(&fs.superblock) {
                if !g0.is_inode_bitmap_uninit() {
                    let expected_inode =
                        ext4_inode_bitmap_csum32(&fs.superblock, &inode_bitmap_data.data);
                    let stored_inode = g0.inode_bitmap_csum();
                    if expected_inode != stored_inode {
                        error!(
                            "Inode bitmap checksum mismatch group=0 expected={expected_inode:#x} \
                             stored={stored_inode:#x}"
                        );
                        return Err(Ext4Error::checksum());
                    }
                }

                if !g0.is_block_bitmap_uninit() {
                    let expected_block =
                        ext4_block_bitmap_csum32(&fs.superblock, &blockbitmap_data.data);
                    let stored_block = g0.block_bitmap_csum();
                    if expected_block != stored_block {
                        error!(
                            "Block bitmap checksum mismatch group=0 expected={expected_block:#x} \
                             stored={stored_block:#x}"
                        );
                        return Err(Ext4Error::checksum());
                    }
                }
            }

            let mut indoe_count: u64 = 0;
            let mut datablock_count: u64 = 0;
            let inode_data_array = &inode_bitmap_data.data;
            let datablock_array = &blockbitmap_data.data;

            inode_data_array.iter().for_each(|&bit| {
                let mut tmp = bit;
                loop {
                    if tmp == 0 {
                        break;
                    }
                    if tmp & 0x1 == 0x1 {
                        indoe_count += 1;
                    }
                    tmp >>= 1;
                }
            });

            datablock_array.iter().for_each(|&bit| {
                let mut tmp = bit;
                loop {
                    if tmp == 0 {
                        break;
                    }
                    if tmp & 0x1 == 0x1 {
                        datablock_count += 1;
                    }
                    tmp >>= 1;
                }
            });

            debug!(
                "Bitmap usage: inodes used = {indoe_count}, data blocks used = {datablock_count}"
            );
        }

        info!("Ext4 filesystem mounted");
        info!("  - block size: {} bytes", fs.superblock.block_size());
        info!("  - total blocks: {}", fs.superblock.blocks_count());
        info!("  - free blocks: {}", fs.superblock.free_blocks_count());
        info!("  - total inodes: {}", fs.superblock.s_inodes_count);
        info!("  - free inodes: {}", fs.superblock.s_free_inodes_count);
        // Flush caches once at the end of mount so any bootstrap repairs are
        // persisted before normal operation begins.
        fs.datablock_cache
            .flush_all(block_dev)
            .expect("flush failed!");
        fs.bitmap_cache.flush_all(block_dev).expect("flush failed!");
        fs.inodetable_cahce
            .flush_all(block_dev)
            .expect("flush failed!");

        Ok(fs)
    }

    /// Loads all block-group descriptors in on-disk order.
    fn load_group_descriptors<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        superblock: &Ext4Superblock,
        group_count: u32,
    ) -> Result<Vec<Ext4GroupDesc>, Ext4Error> {
        let mut group_descs = Vec::new();
        let gdt_base: u64 = BLOCK_SIZE as u64;

        // Cache the currently loaded GDT block to avoid rereading the same
        // block for neighboring descriptors.
        let mut current_block: Option<AbsoluteBN> = None;

        let desc_size = superblock.get_desc_size() as usize;

        debug!("Loading group descriptors: {group_count} groups, desc_size = {desc_size} bytes");
        for group_id in 0..group_count {
            let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
            let block_size_u64 = BLOCK_SIZE as u64;
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
                error!(
                    "GDT out of range: group_id={}, in_block={}, desc_size={}, buffer_len={}",
                    group_id,
                    in_block,
                    desc_size,
                    buffer.len()
                );
                return Err(Ext4Error::bad_superblock());
            }

            let desc = Ext4GroupDesc::from_disk_bytes(&buffer[in_block..end]);
            desc.verify_checksum(superblock, group_id)?;
            group_descs.push(desc);
        }

        debug!(
            "Successfully loaded {} group descriptors",
            group_descs.len()
        );
        Ok(group_descs)
    }
}

/// Thin compatibility wrapper around [`Ext4FileSystem::mount`].
pub fn mount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> Ext4Result<Ext4FileSystem> {
    match Ext4FileSystem::mount(block_dev) {
        Ok(_fs) => {
            info!("Ext4 filesystem mounted");
            Ok(_fs)
        }
        Err(e) => {
            error!("Mount failed: {e}");
            Err(e)
        }
    }
}
