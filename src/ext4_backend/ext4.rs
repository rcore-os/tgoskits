//! Ext4文件系统主调用入口
//!
//! 提供文件系统挂载、卸载、文件操作等高层接口

use crate::ext4_backend::bitmap::InodeBitmap;
use crate::ext4_backend::bitmap_cache::*;
use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::blockgroup_description::*;
use crate::ext4_backend::bmalloc::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::datablock_cache::*;
use crate::ext4_backend::dir::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::inodetable_cache::*;
use crate::ext4_backend::jbd2::jbd2::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::loopfile::*;
use crate::ext4_backend::superblock::*;
use crate::ext4_backend::tool::*;
use crate::ext4_backend::error::*;
use log::trace;

use alloc::collections::vec_deque::VecDeque;
use alloc::vec::Vec;
use log::{debug, error, info, warn};


/// Ext4文件系统实例
/// 管理挂载后的文件系统状态
pub struct Ext4FileSystem {
    /// 超级块
    pub superblock: Ext4Superblock,
    /// 块组描述符数组
    pub group_descs: Vec<Ext4GroupDesc>,
    /// 块分配器
    pub block_allocator: BlockAllocator,
    /// Inode分配器
    pub inode_allocator: InodeAllocator,
    /// 位图缓存（按需加载，LRU淘汰）
    pub bitmap_cache: BitmapCache,
    /// InodeTable缓存
    pub inodetable_cahce: InodeCache,
    /// DataBlock缓存
    pub datablock_cache: DataBlockCache,
    /// 根目录inode号
    pub root_inode: u32,
    /// 块组数量
    pub group_count: u32,
    /// 是否已挂载
    pub mounted: bool,
    /// Journal 超级块 开始块号
    pub journal_sb_block_start: Option<u32>,
}

impl Ext4FileSystem {
    ///对应inode是否已经被分配
    pub fn inode_num_already_allocted<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        inode_num: u64,
    ) -> bool {
        if inode_num == 0 || inode_num > u32::MAX as u64 {
            return false;
        }
        let inode_num = inode_num as u32;

        let (group_idx, inode_in_group) = self.inode_allocator.global_to_group(inode_num);
        let desc = match self.group_descs.get(group_idx as usize) {
            Some(d) => d,
            None => {
                warn!(
                    "inode_num_already_allocted: invalid group_idx {group_idx} for inode {inode_num}"
                );
                return false;
            }
        };
        let bitmap_block = desc.inode_bitmap();
        let cache_key = CacheKey::new_inode(group_idx);

        let bitmap = match self
            .bitmap_cache
            .get_or_load(device, cache_key, bitmap_block)
        {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "inode_num_already_allocted: load inode bitmap failed: {e:?}"
                );
                return false;
            }
        };

        let bm = InodeBitmap::new(&bitmap.data, self.superblock.s_inodes_per_group);
        match bm.is_allocated(inode_in_group) {
            Some(allocated) => allocated,
            None => {
                warn!(
                    "inode_num_already_allocted: inode_in_group {inode_in_group} out of range"
                );
                false
            }
        }
    }

    ///目录是否存在
    pub fn file_entries_exist<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        path: &str,
    ) -> bool {
        let inode = get_file_inode(self, device, path).expect("no dir");
        match &inode {
            Some(inode) => {
                debug!("Find it! Inode:{:?}", &inode);
                true
            }
            None => {
                warn!("Not Find it:(");
                false
            }
        }
    }

    ///遍历目录
    pub fn find_file<B: BlockDevice>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        path: &str,
    ) -> Option<Ext4Inode> {
        let inode = get_file_inode(self, device, path).expect("no dir");
        match &inode {
            Some(inode) => {
                debug!("Found it: {path} !");
                Some(inode.1)
            }
            None => {
                warn!("Not found: {path} !");
                None
            }
        }
    }

    ///获取根目录
    ///上层api封装 获取根目录 inode为2
    pub fn get_root<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<Ext4Inode> {
        let root_inode_num = self.root_inode as u64;
        // 根目录位于块组0的 inode 表中，起始块号从块组描述符读取
        let inode_table_start = match self.group_descs.first() {
            Some(desc) => desc.inode_table(),
            None => return Err(BlockDevError::Corrupted),
        };
        let (block_num, offset, _group_idx) = self.inodetable_cahce.calc_inode_location(
            self.root_inode,
            self.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );
        let result =
            self.inodetable_cahce
                .get_or_load(block_dev, root_inode_num, block_num, offset)?;
        debug!("Root inode i_mode: {}", result.inode.i_mode);
        debug!("Root inode detail: {:?}", result.inode);
        Ok(result.inode)
    }


    ///创建根目录
    ///文件系统初始化时调用
    fn create_root_dir<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<()> {
        // 逻辑迁移到 mkd::create_root_directory_entry 中
        create_root_directory_entry(self, block_dev)
    }

    /// 打开Ext4文件系统
    pub fn mount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> Result<Self, RSEXT4Error> {
        debug!("Start mounting Ext4 filesystem...");

        //在mount时应该重放一遍日志
        //block_dev.set_journal_superblock(super_block, jouranl_start_block);

        // 1. 读取超级块（按 ext4 标准偏移 1024 字节，大小 1024 字节）
        let superblock = read_superblock(block_dev).map_err(|_| RSEXT4Error::IoError)?;

        // 2. 验证魔数
        if superblock.s_magic != EXT4_SUPER_MAGIC {
            error!(
                "Invalid magic: {:#x}, expected: {:#x}",
                superblock.s_magic, EXT4_SUPER_MAGIC
            );
            return Err(RSEXT4Error::InvalidMagic);
        }
        debug!("Superblock magic verified");

        // 3. 检查文件系统状态
        if superblock.s_state == Ext4Superblock::EXT4_ERROR_FS {
            warn!("Filesystem is in error state");
          //  return Err(RSEXT4Error::FilesystemHasErrors);
        }

        // 4. 计算块组数量
        let group_count = superblock.block_groups_count();
        debug!("Block group count: {group_count}");

        // 5. 读取所有块组描述符
        let group_descs =
            Self::load_group_descriptors(block_dev, group_count)?;
        debug!("Loaded {} group descriptors", group_descs.len());

        // 6. 初始化分配器
        let block_allocator = BlockAllocator::new(&superblock);
        let inode_allocator = InodeAllocator::new(&superblock);
        debug!("Allocators initialized");

        // 7. 初始化位图缓存（最多缓存8个位图）
        let bitmap_cache = BitmapCache::default();
        debug!("Bitmap cache initialized (lazy loading)");

        // 初始化inode缓存
        // NOTE: inode size is a filesystem property (superblock.s_inode_size), not a fixed constant.
        // Using a wrong inode size will make inode table offsets incorrect and may read zeroed inodes
        // (e.g. /dev becomes mode=0, then VFS mount fails with ENOTDIR).
        let inode_size = match superblock.s_inode_size {
            0 => DEFAULT_INODE_SIZE as usize,
            n => n as usize,
        };
        let inode_cache = InodeCache::new(INODE_CACHE_MAX, inode_size);
        debug!("Inode cache initialized");

        // 初始化数据块缓存
        let datablock_cache = DataBlockCache::new(DATABLOCK_CACHE_MAX, BLOCK_SIZE);
        debug!("Data block cache initialized");

        // 构造文件系统实例
        let mut fs = Self {
            superblock,
            group_descs,
            block_allocator,
            inode_allocator,
            bitmap_cache,
            root_inode: 2, // Ext4根目录固定为inode 2
            inodetable_cahce: inode_cache,
            datablock_cache,
            group_count,
            mounted: true,
            journal_sb_block_start: None,
        };
        //详细debug输出
        debug_super_and_desc(&fs.superblock, &fs);

        // rootinode check !
        debug!("Checking root directory...");
        {
            let root_inode = fs.get_root(block_dev).map_err(|_| RSEXT4Error::IoError)?;
            if root_inode.i_mode == 0 || !root_inode.is_dir() {
                warn!(
                    "Root inode is uninitialized or not a directory, creating root and lost+found... i_mode: {}, is_dir: {}",
                    root_inode.i_mode,
                    root_inode.is_dir()
                );
                fs.create_root_dir(block_dev)
                    .map_err(|_| RSEXT4Error::IoError)?;
            }
        }

        // lost+found check!
        debug!("Checking lost+found directory...");
        {
            // 1. 优先信任超级块中的 s_lpf_ino（如果非 0）
            if fs.superblock.s_lpf_ino != 0 {
                let ino = fs.superblock.s_lpf_ino;
                debug!("Lost+found inode recorded in superblock: {ino}");
            } else {
                warn!("s_lpf_ino is 0, lost+found not recorded in superblock");
            }

            // 2. 通过路径做一次校验（不会在失败时创建新目录）
            match find_file(&mut fs, block_dev, "/lost+found") {
                Some(_inode) => {
                    info!("/lost+found exists (path resolution)");
                }
                None => {
                    info!("/lost+found not found by path scan;will create!");
                    create_lost_found_directory(&mut fs, block_dev).ok();
                }
            }
        }

        // journal check
        {
            if fs.superblock.has_journal() {
                let mut jouranl_exist: bool = true;
                fs.modify_inode(block_dev, JOURNAL_FILE_INODE as u32, |ji| {
                    jouranl_exist = ji.i_mode != 0;
                })
                .expect("file system error panic!");

                if fs
                    .superblock
                    .has_feature_compat(Ext4Superblock::EXT4_FEATURE_COMPAT_HAS_JOURNAL)
                    && !jouranl_exist
                {
                    // 不存在但 superblock 声明有 journal，则创建一个新的 journal 文件
                    create_journal_entry(&mut fs, block_dev).expect("create journal entry failed");
                    //dump_journal_inode(&mut fs, block_dev);
                }
            }
            //实际启用Journal
            if block_dev.is_use_journal() {
                // 到这里为止：journal inode 一定存在
                // 初始化 jbd2：读入 journal 超级块并塞进 Jbd2Dev
                let mut j_inode = fs
                    .get_inode_by_num(block_dev, JOURNAL_FILE_INODE as u32)
                    .expect("load journal inode failed");

                // 解析 journal inode 第 0 号逻辑块 -> 物理块
                let journal_first_block = resolve_inode_block( block_dev, &mut j_inode, 0)
                    .and_then(|opt| opt.ok_or(BlockDevError::Corrupted))
                    .expect("resolve journal first block failed");

                //写入fs
                fs.journal_sb_block_start = Some(journal_first_block);
                // 通过数据块缓存读出 journal superblock 内容
                let journal_data = fs
                    .datablock_cache
                    .get_or_load(block_dev, journal_first_block as u64)
                    .expect("load journal superblock block failed")
                    .data
                    .clone();

                let j_sb = JournalSuperBllockS::from_disk_bytes(&journal_data);

                // 把 journal superblock 交给 Jbd2Dev，由它内部 lazy-init JBD2DEVSYSTEM
                block_dev.set_journal_superblock(j_sb, fs.journal_sb_block_start.unwrap());

                // Mount-time journal replay for crash recovery.
                block_dev.journal_replay(); //这里是在读取超级块之后再进行回放的，目前为了快速开启日志时数据不一致问题已经在写入超级块，块组描述符时直接落盘
            }
        }

        //详细的Inode/DataBlock占用情况
        {
            let g0 = match fs.group_descs.first() {
                Some(desc) => desc,
                None => return Err(RSEXT4Error::InvalidSuperblock),
            };
            let inode_bitmap_blk = g0.inode_bitmap();
            let data_bitmap_blk = g0.block_bitmap();
            let inode_cache_key = CacheKey::new_inode(0);
            let data_cache_key = CacheKey::new_block(0);

            let inode_bitmap_data = fs
                .bitmap_cache
                .get_or_load(block_dev, inode_cache_key, inode_bitmap_blk as u64)
                .expect("Blcok Read Failed!")
                .clone();
            let blockbitmap_data = fs
                .bitmap_cache
                .get_or_load(block_dev, data_cache_key, data_bitmap_blk as u64)
                .expect("Blcok Read Failed!");

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

        //debug
        // info!(" Ext4文件系统挂载成功！");
        info!("Ext4 filesystem mounted");
        info!("  - block size: {} bytes", fs.superblock.block_size());
        info!("  - total blocks: {}", fs.superblock.blocks_count());
        info!("  - free blocks: {}", fs.superblock.free_blocks_count());
        info!("  - total inodes: {}", fs.superblock.s_inodes_count);
        info!("  - free inodes: {}", fs.superblock.s_free_inodes_count);
        //缓存刷新回磁盘
        fs.datablock_cache
            .flush_all(block_dev)
            .expect("flush failed!");
        fs.bitmap_cache.flush_all(block_dev).expect("flush failed!");
        fs.inodetable_cahce
            .flush_all(block_dev)
            .expect("flush failed!");

        Ok(fs)
    }

    /// 加载所有块组描述符 顺序性
    fn load_group_descriptors<B: BlockDevice>(
        block_dev: &mut Jbd2Dev<B>,
        group_count: u32,
    ) -> Result<Vec<Ext4GroupDesc>, RSEXT4Error> {
        let mut group_descs = Vec::new();
        let gdt_base: u64 = BLOCK_SIZE as u64;

        // 为了减少重复读块，这里缓存当前块号
        let mut current_block: Option<u64> = None;

        let superblock = read_superblock(block_dev).map_err(|_| RSEXT4Error::IoError)?;
        let desc_size = superblock.get_desc_size() as usize;

        debug!(
            "Loading group descriptors: {group_count} groups, desc_size = {desc_size} bytes"
        );
        for group_id in 0..group_count {
            let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
            let block_size_u64 = BLOCK_SIZE as u64;
            let block_num = byte_offset / block_size_u64;
            let in_block = (byte_offset % block_size_u64) as usize;

            // 只在块号变化时重新读取块
            if current_block != Some(block_num) {
                block_dev
                    .read_block(block_num as u32)
                    .map_err(|_| RSEXT4Error::IoError)?;
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
                return Err(RSEXT4Error::InvalidSuperblock);
            }

            let desc = Ext4GroupDesc::from_disk_bytes(&buffer[in_block..end]);
            group_descs.push(desc);
        }

        debug!(
            "Successfully loaded {} group descriptors",
            group_descs.len()
        );
        Ok(group_descs)
    }
    /// 卸载文件系统 不写超级块备份
    pub fn umount<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<()> {
        if !self.mounted {
            return Ok(());
        }

        debug!("Unmounting Ext4 filesystem...");

        // 1. Flush dirty caches
        info!("Flushing bitmap cache...");
        self.bitmap_cache.flush_all(block_dev)?;
        debug!("Bitmap cache flushed");
        self.inodetable_cahce.flush_all(block_dev)?;
        debug!("Inode table cache flushed");
        self.datablock_cache.flush_all(block_dev)?;
        debug!("Data block cache flushed");


        // 4. Update superblock
        info!("Writing back superblock...");
        self.sync_superblock(block_dev)?;
        debug!("Superblock updated");

        // Write back group descriptors
        debug!("Writing back group descriptors...");
        self.sync_group_descriptors(block_dev)?;

        //确保缓存已经提交完毕
        block_dev.umount_commit();
       

        self.mounted = false;
        info!("Filesystem unmounted cleanly");

        Ok(())
    }

    /// 同步块组描述符到磁盘
    /// 按 ext4 标准布局，将所有块组描述符写回：
    /// GDT 字节流紧跟在超级块之后
    pub fn sync_group_descriptors<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<()> {
        let total_desc_count = self.group_descs.len();
        let desc_size = self.superblock.get_desc_size() as usize;

        // GDT 基地址统一为块号 1 的起始字节偏移
        let gdt_base: u64 = BLOCK_SIZE as u64;
        let block_size_u64 = BLOCK_SIZE as u64;

        debug!(
            "Writing back group descriptors: {total_desc_count} descriptors, desc_size = {desc_size} bytes"
        );

        // 为了避免频繁读写，按块聚合写回
        let mut current_block: Option<u64> = None;
        let mut buffer_snapshot_block: Option<u64> = None;

        for (idx, desc) in self.group_descs.iter().enumerate() {
            let byte_offset = gdt_base + idx as u64 * desc_size as u64;
            let block_num = byte_offset / block_size_u64;
            let in_block = (byte_offset % block_size_u64) as usize;
            let end = in_block + desc_size;

            // 如果块号变化，先把前一个块写回
            if current_block != Some(block_num) {
                if let Some(prev_block) = current_block
                    && Some(prev_block) == buffer_snapshot_block {
                        //由于目前日志回放在fs构建之后（块组描述符读取之后），目前为了快速修复防止读取到旧的超级块。直接落盘写回
                        block_dev.write_block(prev_block as u32, false)?;
                    }

                // 读取新块
                block_dev.read_block(block_num as u32)?;
                current_block = Some(block_num);
                buffer_snapshot_block = Some(block_num);
            }

            let buffer = block_dev.buffer_mut();
            if end > buffer.len() {
                error!(
                    "GDT out of range: idx={}, in_block={}, desc_size={}, buffer_len={}",
                    idx,
                    in_block,
                    desc_size,
                    buffer.len()
                );
                return Err(BlockDevError::Corrupted);
            }

            desc.to_disk_bytes(&mut buffer[in_block..end]);
        }

        // 写回最后一个块
        if let Some(last_block) = current_block
            && Some(last_block) == buffer_snapshot_block {
                block_dev.write_block(last_block as u32, true)?;
            }

        debug!("Group descriptors written back");
        Ok(())
    }

    /// 同时修改所有需要冗余备份的块组
    /// 同步超级块到磁盘
    pub fn sync_superblock<B: BlockDevice>(&mut self, block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<()> {
        //同步group_desc 和 super_block计数
        let mut real_free_blocks: u64 = 0;
        let mut real_free_inodes: u64 = 0;
        for desc in &self.group_descs {
            real_free_blocks += desc.free_blocks_count() as u64;
            real_free_inodes += desc.free_inodes_count() as u64;
        }
        self.superblock.s_free_blocks_count_lo = (real_free_blocks & 0xFFFFFFFF) as u32;
        self.superblock.s_free_blocks_count_hi = (real_free_blocks >> 32) as u32;
        self.superblock.s_free_inodes_count = real_free_inodes as u32;

        write_superblock(block_dev, &self.superblock)
    }

    /// 获取块组描述符
    pub fn get_group_desc(&self, group_idx: u32) -> Option<&Ext4GroupDesc> {
        self.group_descs.get(group_idx as usize)
    }

    /// 获取可变块组描述符
    pub fn get_group_desc_mut(&mut self, group_idx: u32) -> Option<&mut Ext4GroupDesc> {
        self.group_descs.get_mut(group_idx as usize)
    }

    /// 使用闭包修改指定 inode，内部自动计算 inode 在磁盘上的位置
    pub fn modify_inode<B, F>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u32,
        f: F,
    ) -> BlockDevResult<()>
    where
        B: BlockDevice,
        F: FnOnce(&mut Ext4Inode),
    {
        // 通过全局 inode 号计算所属块组
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num);

        let inode_table_start = self
            .group_descs
            .get(group_idx as usize)
            .ok_or(BlockDevError::Corrupted)?
            .inode_table();

        let (block_num, offset, _g) = self.inodetable_cahce.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );

        self.inodetable_cahce
            .modify(block_dev, inode_num as u64, block_num, offset, f)
    }

    /// 按 inode 号加载 inode（只读），内部自动计算在磁盘上的位置
    pub fn get_inode_by_num<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u32,
    ) -> BlockDevResult<Ext4Inode> {
        let (group_idx, _idx_in_group) = self.inode_allocator.global_to_group(inode_num);

        let inode_table_start = self
            .group_descs
            .get(group_idx as usize)
            .ok_or(BlockDevError::Corrupted)?
            .inode_table();

        let (block_num, offset, _g) = self.inodetable_cahce.calc_inode_location(
            inode_num,
            self.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );

        let cached =
            self.inodetable_cahce
                .get_or_load(block_dev, inode_num as u64, block_num, offset)?;
        Ok(cached.inode)
    }

    /// 在整个文件系统中分配指定数量的连续数据块
    pub fn alloc_blocks<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> BlockDevResult<Vec<u64>> {
        if count == 0 {
            return Ok(Vec::new());
        }

        trace!(
            "alloc_blocks: request count={count} (will scan groups for free space)"
        );

        // 选择一个有足够空闲块的块组，并在该组内做连续分配
        for (idx, desc) in self.group_descs.iter().enumerate() {
            let group_idx = idx as u32;
            let free = desc.free_blocks_count();

            trace!(
                "alloc_blocks: inspect group={group_idx} free_blocks={free} need={count}"
            );

            if free < count {
                continue;
            }

            let bitmap_block = desc.block_bitmap();
            let cache_key = CacheKey::new_block(group_idx);
            let mut alloc_res: Result<BlockAlloc, BlockDevError> = Err(BlockDevError::NoSpace);

            debug!(
                "alloc_blocks: candidate group={group_idx} bitmap_block={bitmap_block} starting contiguous allocation of {count} blocks"
            );

            self.bitmap_cache
                .modify(block_dev, cache_key, bitmap_block, |data| {
                    // 这里只修改位图，不直接接触 group_desc / superblock 计数
                    let r = self
                        .block_allocator
                        .alloc_contiguous_blocks(data, group_idx, count);
                    alloc_res = r.map_err(|_| BlockDevError::NoSpace);
                })?;

            let alloc = alloc_res?;

            // 更新块组描述符
            if let Some(desc_mut) = self.get_group_desc_mut(group_idx) {
                let before = desc_mut.free_blocks_count();
                let new_count = before.saturating_sub(count);
                desc_mut.bg_free_blocks_count_lo = (new_count & 0xFFFF) as u16;
                desc_mut.bg_free_blocks_count_hi = (new_count >> 16) as u16;

                debug!(
                    "alloc_blocks: group={} free_blocks_count change {} -> {} (allocated {} blocks starting at global={})",
                    group_idx, before, new_count, count, alloc.global_block
                );
            }

            // 更新超级块
            let sb_before = self.superblock.free_blocks_count();
            self.superblock.s_free_blocks_count_lo =
                self.superblock.s_free_blocks_count_lo.saturating_sub(count);
            let sb_after = self.superblock.free_blocks_count();

            debug!(
                "alloc_blocks: superblock free_blocks_count change {sb_before} -> {sb_after} (delta=-{count})"
            );

            let mut blocks = Vec::with_capacity(count as usize);
            for off in 0..count {
                blocks.push(alloc.global_block + off as u64);
            }

            debug!(
                "Allocated blocks: group={}, first_block_in_group={}, first_global_block={}, count={} [bitmap updated, writeback deferred]",
                alloc.group_idx, alloc.block_in_group, alloc.global_block, count
            );

            return Ok(blocks);
        }

        debug!(
            "alloc_blocks: no group has enough free blocks for request count={count}"
        );

        Err(BlockDevError::NoSpace)
    }

    /// 在整个文件系统中分配一个数据块（兼容旧接口）
    pub fn alloc_block<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<u64> {
        let mut v = self.alloc_blocks(block_dev, 1)?;
        Ok(v.pop().unwrap())
    }

    /// 在整个文件系统中分配指定数量的 inode
    pub fn alloc_inodes<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> BlockDevResult<Vec<u32>> {
        if count == 0 {
            return Ok(Vec::new());
        }

        // 目前按“同一块组内尽量连续”策略，从第一个有足够空闲 inode 的组开始分配
        for (idx, desc) in self.group_descs.iter().enumerate() {
            let group_idx = idx as u32;
            let free = desc.free_inodes_count();
            if free < count {
                continue;
            }

            let bitmap_block = desc.inode_bitmap();
            let cache_key = CacheKey::new_inode(group_idx);

            let mut inodes: Vec<u32> = Vec::with_capacity(count as usize);

            self.bitmap_cache
                .modify(block_dev, cache_key, bitmap_block, |data| {
                    // 简化实现：在同一块组中循环调用 alloc_inode_in_group，得到 count 个 inode
                    for _ in 0..count {
                        let r = self
                            .inode_allocator
                            .alloc_inode_in_group(data, group_idx, desc);
                        match r {
                            Ok(InodeAlloc { global_inode, .. }) => {
                                inodes.push(global_inode);
                            }
                            Err(_) => {
                                break;
                            }
                        }
                    }
                })?;

            if inodes.len() as u32 != count {
                return Err(BlockDevError::NoSpace);
            }

            // 更新块组描述符
            if let Some(desc_mut) = self.get_group_desc_mut(group_idx) {
                let new_count = desc_mut.free_inodes_count().saturating_sub(count);
                desc_mut.bg_free_inodes_count_lo = (new_count & 0xFFFF) as u16;
                desc_mut.bg_free_inodes_count_hi = (new_count >> 16) as u16;
            }

            // 更新超级块
            self.superblock.s_free_inodes_count =
                self.superblock.s_free_inodes_count.saturating_sub(count);

            debug!(
                "Allocated inodes: group={}, first_global_inode={}, count={} [delayed write]",
                group_idx, inodes[0], count
            );

            return Ok(inodes);
        }

        Err(BlockDevError::NoSpace)
    }

    /// 在整个文件系统中分配一个 inode（兼容旧接口）
    pub fn alloc_inode<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
    ) -> BlockDevResult<u32> {
        let mut v = self.alloc_inodes(block_dev, 1)?;
        Ok(v.pop().unwrap())
    }

    /// 根据全局物理块号释放一个数据块
    /// 内部自动计算所属块组和位图位置，并更新块组/超级块计数
    pub fn free_block<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        global_block: u64,
    ) -> BlockDevResult<()> {
        // 通过 BlockAllocator 反推 (group_idx, block_in_group)
        let (group_idx, block_in_group) = self.block_allocator.global_to_group(global_block);
        let bitmap_block;
        let cache_key;
        // 获取对应块组描述符
        {
            let desc = self
                .get_group_desc_mut(group_idx)
                .ok_or(BlockDevError::Corrupted)?;
            bitmap_block = desc.block_bitmap();
            cache_key = CacheKey::new_block(group_idx);
        }
        // 在位图上清零对应 bit
        // Note: freeing the same block twice should not bring the whole filesystem down.
        // Treat AlreadyFree as a no-op.
        let mut free_ok = Ok(());
        let mut did_free = true;
        self.bitmap_cache
            .modify(block_dev, cache_key, bitmap_block, |data| {
                free_ok = match self.block_allocator.free_block(data, block_in_group) {
                    Ok(()) => Ok(()),
                    Err(crate::ext4_backend::bmalloc::AllocError::BitmapError(
                        crate::ext4_backend::bitmap::BitmapError::AlreadyFree,
                    )) => {
                        did_free = false;
                        Ok(())
                    }
                    Err(_) => Err(BlockDevError::Corrupted),
                };
            })?;
        free_ok?;

        if !did_free {
            return Ok(());
        }
        let desc = self
            .get_group_desc_mut(group_idx)
            .ok_or(BlockDevError::Corrupted)?;
        // 更新块组 free_blocks_count
        let before = desc.free_blocks_count();
        let new_count = before.saturating_add(1);
        desc.bg_free_blocks_count_lo = (new_count & 0xFFFF) as u16;
        desc.bg_free_blocks_count_hi = (new_count >> 16) as u16;

        // 更新超级块 free_blocks_count
        self.superblock.s_free_blocks_count_lo =
            self.superblock.s_free_blocks_count_lo.saturating_add(1);
        Ok(())
    }

    /// 根据 inode 号释放一个 inode
    /// 内部自动计算所属块组和位图位置，并更新块组/超级块计数
    pub fn free_inode<B: BlockDevice>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        inode_num: u32,
    ) -> BlockDevResult<()> {
        // 通过 InodeAllocator 反推 (group_idx, inode_in_group)
        let (group_idx, inode_in_group) = self.inode_allocator.global_to_group(inode_num);
        let bitmap_block;
        let cache_key;
        // 获取对应块组描述符
        {
            let desc = self
                .get_group_desc_mut(group_idx)
                .ok_or(BlockDevError::Corrupted)?;
            bitmap_block = desc.inode_bitmap();
            cache_key = CacheKey::new_inode(group_idx);
        }

        let mut free_ok = Ok(());
        let mut did_free = true;
        self.bitmap_cache
            .modify(block_dev, cache_key, bitmap_block, |data| {
                free_ok = match self.inode_allocator.free_inode(data, inode_in_group) {
                    Ok(()) => Ok(()),
                    Err(crate::ext4_backend::bmalloc::AllocError::BitmapError(
                        crate::ext4_backend::bitmap::BitmapError::AlreadyFree,
                    )) => {
                        did_free = false;
                        Ok(())
                    }
                    Err(_) => Err(BlockDevError::Corrupted),
                };
            })?;
        free_ok?;

        if !did_free {
            return Ok(());
        }

        let desc = self
            .get_group_desc_mut(group_idx)
            .ok_or(BlockDevError::Corrupted)?;
        // 更新块组 free_inodes_count
        let before = desc.free_inodes_count();
        let new_count = before.saturating_add(1);
        desc.bg_free_inodes_count_lo = (new_count & 0xFFFF) as u16;
        desc.bg_free_inodes_count_hi = (new_count >> 16) as u16;

        // 更新超级块 free_inodes_count
        self.superblock.s_free_inodes_count = self.superblock.s_free_inodes_count.saturating_add(1);
        // 真正清空inodetable 大坑....，free_inode必须清空inodetable。不然e2fsck会捣蛋
        self.modify_inode(block_dev, inode_num, |td| *td = Ext4Inode::default())?;
        Ok(())
    }

    /// 查找有空闲块的块组
    pub fn find_group_with_free_blocks(&self) -> Option<u32> {
        for (idx, desc) in self.group_descs.iter().enumerate() {
            if desc.free_blocks_count() > 0 {
                return Some(idx as u32);
            }
        }
        None
    }

    /// 查找有空闲inode的块组
    pub fn find_group_with_free_inodes(&self) -> Option<u32> {
        for (idx, desc) in self.group_descs.iter().enumerate() {
            if desc.free_inodes_count() > 0 {
                return Some(idx as u32);
            }
        }
        None
    }

    /// 获取文件系统统计信息
    pub fn statfs(&self) -> FileSystemStats {
        FileSystemStats {
            total_blocks: self.superblock.blocks_count(),
            free_blocks: self.superblock.free_blocks_count(),
            total_inodes: self.superblock.s_inodes_count,
            free_inodes: self.superblock.s_free_inodes_count,
            block_size: self.superblock.block_size(),
            block_groups: self.group_count,
        }
    }

    ///创建最基本的file
    pub fn make_base_dir(&self) {
        //root journal lost+found
    }
}

/// 文件系统统计信息
#[derive(Debug, Clone, Copy)]
pub struct FileSystemStats {
    /// 总块数
    pub total_blocks: u64,
    /// 空闲块数
    pub free_blocks: u64,
    /// 总inode数
    pub total_inodes: u32,
    /// 空闲inode数
    pub free_inodes: u32,
    /// 块大小（字节）
    pub block_size: u64,
    /// 块组数
    pub block_groups: u32,
}
///entries是否存在
pub fn file_entry_exisr<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    path: &str,
) -> bool {
    fs.file_entries_exist(device, path)
}
/// 文件寻找函数-线性扫描
pub fn find_file<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    device: &mut Jbd2Dev<B>,
    path: &str,
) -> Option<Ext4Inode> {
    fs.find_file(device, path)
}

/// 简化的挂载函数（用于兼容旧代码）
pub fn mount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<Ext4FileSystem> {
    match Ext4FileSystem::mount(block_dev) {
        Ok(_fs) => {
            info!("Ext4 filesystem mounted");
            Ok(_fs)
        }
        Err(e) => {
            error!("Mount failed: {e}");
            Err(BlockDevError::Corrupted)
        }
    }
}

///取消挂载函数
pub fn umount<B: BlockDevice>(
    fs: Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
) -> BlockDevResult<()> {
    let mut f = fs;
    f.umount(block_dev)?;
    Ok(())
}

/// 文件系统布局信息（仅用于 mkfs 阶段的计算）
pub struct FsLayoutInfo {
    /// 逻辑块大小（字节）
    block_size: u32,
    /// 每组块数
    blocks_per_group: u32,
    /// 每组 inode 数
    inodes_per_group: u32,
    /// inode 大小（字节）
    inode_size: u16,
    /// 块组数
    groups: u32,
    /// 块组描述符大小（字节）
    desc_size: u16,
    /// 每块能容纳的组描述符个数
    descs_per_block: u32,
    /// 主 GDT 实际占用的块数
    gdt_blocks: u32,
    /// 每组 inode 表占用的块数
    inode_table_blocks: u32,
    /// 第一个数据块号（对应 s_first_data_block）
    first_data_block: u32,
    /// 预留的 GDT 块数（应等于 RESERVED_GDT_BLOCKS）
    reserved_gdt_blocks: u32,
    /// 组0的块位图块号
    group0_block_bitmap: u32,
    /// 组0的 inode 位图块号
    group0_inode_bitmap: u32,
    /// 组0的 inode 表起始块号
    group0_inode_table: u32,
    /// 组0中元数据占用的块数
    group0_metadata_blocks: u32,
    /// 预留块总数（按比例预留给 root）
    reserved_blocks: u64,
}

/// block_group 布局信息，仅在 mkfs 阶段使用
pub struct BlcokGroupLayout {
    /// 块组起始块号（全局块号）
    pub group_start_block: u64,
    /// 块组内块位图所在的块号（全局块号）
    pub group_blcok_bitmap_startblocks: u64,
    /// 块组内 inode 位图所在的块号（全局块号）
    pub group_inode_bitmap_startblocks: u64,
    /// 块组内 inode 表起始块号（全局块号）
    pub group_inode_table_startblocks: u64,
    /// 该块组中元数据占用的块数（引导/备份 super+GDT+位图+inode 表）
    pub metadata_blocks_in_group: u32,
}

pub fn compute_fs_layout(inode_size:u16,total_blocks: u64) -> FsLayoutInfo {
    let block_size: u32 = 1024u32 << LOG_BLOCK_SIZE;

    // 每组块数：8 * block_size（标准 ext4 默认）
    let blocks_per_group: u32 = 8 * block_size;

    // 每组 inode 数：blocks_per_group / 4（简化策略）
    let inodes_per_group: u32 = blocks_per_group / 4;

    // 块组数：向上取整
    let groups: u32 =
        total_blocks.div_ceil(blocks_per_group as u64) as u32;

    // 确定块组描述符大小，默认使用64位描述符大小，除非明确指定使用32位
    let desc_size: u16 = if DEFAULT_FEATURE_INCOMPAT & Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT != 0 {
        GROUP_DESC_SIZE
    } else {
        GROUP_DESC_SIZE_OLD
    };

    // 每块能容纳的组描述符个数
    let descs_per_block: u32 = if desc_size == 0 {
        0
    } else {
        block_size / desc_size as u32
    };

    // GDT 实际占用的块数
    let gdt_blocks: u32 = if descs_per_block == 0 {
        0
    } else {
        groups.div_ceil(descs_per_block)
    };

    // 每组 inode 表占用的块数
    let inode_table_blocks: u32 = if block_size == 0 {
        0
    } else {
        (inodes_per_group * inode_size as u32).div_ceil(block_size)
    };

    // 第一个数据块：块大小 > 1024 时为 0，否则为 1（参考 lwext4 create_fs_aux_info）
    let first_data_block: u32 = if block_size > 1024 { 0 } else { 1 };

    // 预留的 GDT 块数（与 ext4 标准一致）
    let reserved_gdt_blocks: u32 = RESERVED_GDT_BLOCKS;

    // 组0布局：
    // - 对于 4K：Primary superblock at 0, GDT at 1, Reserved GDT blocks at 2..(2+reserved_gdt_blocks-1)
    // - 我们在预留 GDT 区域之后顺序放置 block_bitmap、inode_bitmap、inode_table
    let group0_start: u32 = first_data_block;
    let reserved_gdt_start: u32 = group0_start + 2; // 块0=引导/超级块，块1=GDT，块2.. 预留GDT
    let group0_block_bitmap: u32 = reserved_gdt_start + reserved_gdt_blocks; // 2 + reserved
    let group0_inode_bitmap: u32 = group0_block_bitmap + 1;
    let group0_inode_table: u32 = group0_inode_bitmap + 1;
    let group0_metadata_blocks: u32 = (group0_inode_table + inode_table_blocks) - group0_start;

    // 预留块总数：约 5%（与 ext4 默认类似）
    let reserved_blocks: u64 = total_blocks / 20; // 5%

    FsLayoutInfo {
        block_size,
        blocks_per_group,
        inodes_per_group,
        inode_size,
        groups,
        desc_size,
        descs_per_block,
        gdt_blocks,
        inode_table_blocks,
        first_data_block,
        reserved_gdt_blocks,
        group0_block_bitmap,
        group0_inode_bitmap,
        group0_inode_table,
        group0_metadata_blocks,
        reserved_blocks,
    }
}

pub fn mkfs<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<()> {
    debug!("Start initializing Ext4 filesystem...");
    // mkfs 阶段先强制关闭日志，避免还未初始化 journal superblock 时触发 JBD2 逻辑
    block_dev.set_journal_use(false);
    let old_jouranl_use = block_dev.is_use_journal();

    // 1. 计算布局参数
    let total_blocks = block_dev.total_blocks();
    let layout = compute_fs_layout(DEFAULT_INODE_SIZE,total_blocks);
    let total_groups = layout.groups;

    debug!("  Total blocks: {total_blocks}");
    debug!("  Block size: {} bytes", layout.block_size);
    debug!("  Block group count: {total_groups}");
    debug!("  Blocks per group: {}", layout.blocks_per_group);
    debug!("  Inodes per group: {}", layout.inodes_per_group);

    //构建并根据fearure写入到所有group超级块
    let superblock = build_superblock(total_blocks, &layout);
    write_superblock(block_dev, &superblock)?;
    debug!("Superblock written");

    //写冗余备份 自动判断是否写
    write_superblock_redundant_backup(block_dev, &superblock, total_groups, &layout)?;

    //注意顺序
    let mut descs: VecDeque<Ext4GroupDesc> = VecDeque::new();
    //为superblock写入gdt（全部标记为UNINIT）
    for group_id in 0..total_groups {
        let desc = build_uninit_group_desc(&superblock, group_id, &layout);
        write_group_desc(block_dev, group_id, &desc)?;
        descs.push_back(desc);
    }
    //为其它块组选择性的写入冗余备份desc
    write_gdt_redundant_backup(block_dev, &descs, &superblock, total_groups, &layout)?;
    debug!("{total_groups} block group descriptors written");

    //实际初始化块组0（用于根目录）
    initialize_group_0(block_dev, &layout)?;
    debug!("Block group 0 initialized (for root directory)");

    // 初始化其它块组的位图（全部视为空闲）
    initialize_other_groups_bitmaps(block_dev, &layout, &superblock)?;

    //通过一次挂载/卸载流程，让根目录在 mkfs 阶段就被真正创建并写回磁盘
    // 注意：此时日志仍然关闭，等真正挂载时再开启 JBD2
    {
        let mut fs = Ext4FileSystem::mount(block_dev).expect("Mount Failed!");
        fs.umount(block_dev)?;
    }

    //  验证：读回超级块检查魔数
    let verify_sb = read_superblock(block_dev)?;

    // mkfs 结束前恢复日志开关（为后续真实挂载做准备）
    block_dev.set_journal_use(old_jouranl_use);

    if verify_sb.s_magic == EXT4_SUPER_MAGIC {
        debug!(
            "Format completed, superblock magic verified: {:#x}",
            verify_sb.s_magic
        );
        Ok(())
    } else {
        debug!("Superblock magic verification failed");
        Err(BlockDevError::Corrupted)
    }
}

/// 构建超级块 不管字节序
fn build_superblock(total_blocks: u64, layout: &FsLayoutInfo) -> Ext4Superblock {
    let mut sb = Ext4Superblock::default();

    // 魔数
    sb.s_magic = EXT4_SUPER_MAGIC;

    // 块信息
    sb.s_blocks_count_lo = (total_blocks & 0xFFFFFFFF) as u32;
    sb.s_blocks_count_hi = (total_blocks >> 32) as u32;

    // Ext4 标准：块大小 = 1024 << s_log_block_size
    sb.s_log_block_size = LOG_BLOCK_SIZE;
    // 簇大小目前与块大小一致
    sb.s_log_cluster_size = LOG_BLOCK_SIZE;

    // 每组块数 / inode 数量
    sb.s_blocks_per_group = layout.blocks_per_group;
    sb.s_inodes_per_group = layout.inodes_per_group;
    // 与块大小一致的簇配置：clusters_per_group = blocks_per_group
    sb.s_clusters_per_group = layout.blocks_per_group;

    // inode 信息
    sb.s_inodes_count = layout.groups * layout.inodes_per_group;
    sb.s_inode_size = layout.inode_size;
    // 第一个非保留 inode（通常为 11 = 保留 1..10）
    sb.s_first_ino = RESERVED_INODES + 1;

    // 第一个数据块
    sb.s_first_data_block = layout.first_data_block;

    // 预留块数（低/高 32 位）
    sb.s_r_blocks_count_lo = (layout.reserved_blocks & 0xFFFFFFFF) as u32;
    sb.s_r_blocks_count_hi = (layout.reserved_blocks >> 32) as u32;

    //设置hash种子
    //需要生成UUID
    let uuid = generate_uuid();
    sb.s_hash_seed = uuid.0;

    //设置文件系统UUID
    let filesys_uuid = generate_uuid_8();
    sb.s_uuid = filesys_uuid;

    // 空闲计数：总块数 - 组0元数据块数 - 预留块数（其余组初始全空闲）
    let metadata_blocks = layout.group0_metadata_blocks as u64;
    let mut free_blocks = total_blocks
        .saturating_sub(metadata_blocks)
        .saturating_sub(layout.reserved_blocks);
    if free_blocks > total_blocks {
        free_blocks = 0;
    }
    sb.s_free_blocks_count_lo = (free_blocks & 0xFFFFFFFF) as u32;
    sb.s_free_blocks_count_hi = (free_blocks >> 32) as u32;

    sb.s_min_extra_isize = 32;
    sb.s_want_extra_isize = 32;

    // 预留 inode（1-RESERVED_INODES）不可用
    sb.s_free_inodes_count = sb.s_inodes_count.saturating_sub(RESERVED_INODES);

    // 文件系统状态与错误处理（参考 lwext4 fill_sb）
    sb.s_state = Ext4Superblock::EXT4_VALID_FS;
    sb.s_errors = Ext4Superblock::EXT4_ERRORS_RO;

    // 创建者 OS / 版本号
    sb.s_creator_os = Ext4Superblock::EXT4_OS_LINUX;
    sb.s_rev_level = Ext4Superblock::EXT4_DYNAMIC_REV;

    // 特性标志
    sb.s_feature_compat = DEFAULT_FEATURE_COMPAT;
    sb.s_feature_incompat = DEFAULT_FEATURE_INCOMPAT;
    sb.s_feature_ro_compat = DEFAULT_FEATURE_RO_COMPAT;

    // 块组描述符大小
    sb.s_desc_size = layout.desc_size;
    // 预留的 GDT 块数（仅 mkfs 默认值，挂载时应相信磁盘中的值）
    sb.s_reserved_gdt_blocks = layout.reserved_gdt_blocks as u16;

    sb
}

/// 构建未初始化的块组描述符 不管字节序
fn build_uninit_group_desc(
    sb: &Ext4Superblock,
    group_id: u32,
    layout: &FsLayoutInfo,
) -> Ext4GroupDesc {
    let mut desc = Ext4GroupDesc::default();

    // 通过工具函数统一计算该块组的布局
    let gl = cloc_group_layout(
        group_id,
        sb,
        layout.blocks_per_group,
        layout.inode_table_blocks,
        layout.group0_block_bitmap,
        layout.group0_inode_bitmap,
        layout.group0_inode_table,
        layout.gdt_blocks,
    );

    // 位图和 inode 表块号
    desc.bg_block_bitmap_lo = gl.group_blcok_bitmap_startblocks as u32;
    desc.bg_inode_bitmap_lo = gl.group_inode_bitmap_startblocks as u32;
    desc.bg_inode_table_lo = gl.group_inode_table_startblocks as u32;

    // 理论空闲块数：整组减去元数据块
    let used_meta = gl.metadata_blocks_in_group as u32;
    let free_blocks = layout.blocks_per_group.saturating_sub(used_meta);

    if group_id == 0 {
        // 组0 还需要扣掉保留 inode
        desc.bg_free_blocks_count_lo = free_blocks as u16;
        desc.bg_free_inodes_count_lo =
            layout.inodes_per_group.saturating_sub(RESERVED_INODES) as u16;
    } else {
        desc.bg_free_blocks_count_lo = free_blocks as u16;
        desc.bg_free_inodes_count_lo = layout.inodes_per_group as u16;
    }

    // 目前不使用高 16 位计数和 UNINIT 标志
    desc.bg_free_blocks_count_hi = 0;
    desc.bg_free_inodes_count_hi = 0;
    desc.bg_used_dirs_count_lo = 0;
    desc.bg_used_dirs_count_hi = 0;
    desc.bg_flags = 0;

    desc
}

///写备份超级块到所有组，从块组1开始
fn write_superblock_redundant_backup<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    sb: &Ext4Superblock,
    groups_count: u32,
    fs_layout: &FsLayoutInfo,
) -> BlockDevResult<()> {
    //从1开始
    // sparse_superbllock特性判断
    let sprse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sprse_feature {
        for gid in 1..groups_count {
            let group_layout = cloc_group_layout(
                gid,
                sb,
                fs_layout.blocks_per_group,
                fs_layout.inode_table_blocks,
                fs_layout.group0_block_bitmap,
                fs_layout.group0_inode_bitmap,
                fs_layout.group0_inode_table,
                fs_layout.gdt_blocks,
            );
            //需要超级块备份
            if need_redundant_backup(gid) {
                let super_blocks = group_layout.group_start_block;
                block_dev.read_block(super_blocks as u32).expect("Superblock read failed!");
                let buffer = block_dev.buffer_mut();
                sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
                block_dev.write_block(super_blocks as u32, true)?;
            }
        }
    }
    Ok(())
}

/// 写入超级块到磁盘 管字节序 不写备份
fn write_superblock<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    sb: &Ext4Superblock,
) -> BlockDevResult<()> {
    // 超级块总是从分区偏移 1024 字节开始，占用 1024 字节
    if BLOCK_SIZE == 1024 {
        block_dev.read_block(1)?;
        let buffer = block_dev.buffer_mut();
        sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
        block_dev.write_block(1, true)?;
    } else {
        block_dev.read_block(0)?;
        let buffer = block_dev.buffer_mut();
        let offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize; // 1024
        let end = offset + Ext4Superblock::SUPERBLOCK_SIZE;
        sb.to_disk_bytes(&mut buffer[offset..end]);
        block_dev.write_block(0, false)?; //由于目前日志回放在超级块读取后，目前为了快速修复防止读取到旧的超级块。直接让超级块落盘写回
    }

    Ok(())
}

/// 读取超级块 管字节序
fn read_superblock<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> BlockDevResult<Ext4Superblock> {
    // 超级块总是从分区偏移 1024 字节开始，占用 1024 字节
    // 这里通过按 BLOCK_SIZE 读块，再在块内做 1024 字节切片来解析
    if BLOCK_SIZE == 1024 {
        block_dev.read_block(1)?;
        let buffer = block_dev.buffer();
        let sb = Ext4Superblock::from_disk_bytes(&buffer[0..SUPERBLOCK_SIZE]);
        Ok(sb)
    } else {
        block_dev.read_block(0)?;
        let buffer = block_dev.buffer();
        let offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize; // 1024
        let end = offset + Ext4Superblock::SUPERBLOCK_SIZE;
        let sb = Ext4Superblock::from_disk_bytes(&buffer[offset..end]);
        Ok(sb)
    }
}

///写入所有组的冗余备份中 自动判断特性
fn write_gdt_redundant_backup<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    descs: &VecDeque<Ext4GroupDesc>,
    sb: &Ext4Superblock,
    groups_count: u32,
    fs_layout: &FsLayoutInfo,
) -> BlockDevResult<()> {
    //参数合法性判断
    let desc_size = sb.get_desc_size();
    let desc_all_size = descs.len() * desc_size as usize;
    let can_recive_size = fs_layout.gdt_blocks * fs_layout.descs_per_block * desc_size as u32;
    if can_recive_size < desc_all_size as u32 {
        return Err(BlockDevError::BufferTooSmall {
            provided: can_recive_size as usize,
            required: desc_all_size,
        });
    }

    let sprse_feature =
        sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sprse_feature {
        //为每个块组执行
        for gid in 1..groups_count {
            if need_redundant_backup(gid) {
                let group_layout = cloc_group_layout(
                    gid,
                    sb,
                    fs_layout.blocks_per_group,
                    fs_layout.inode_table_blocks,
                    fs_layout.group0_block_bitmap,
                    fs_layout.group0_inode_bitmap,
                    fs_layout.group0_inode_table,
                    fs_layout.gdt_blocks,
                );
                let gdt_start = group_layout.group_start_block + 1; //跳过超级块

                let mut desc_iter = descs.iter();
                //循环写入desc
                for gdt_block_id in gdt_start..group_layout.group_blcok_bitmap_startblocks {
                    block_dev.read_block(gdt_block_id as u32)?;
                    let buffer = block_dev.buffer_mut();
                    let mut current_offset = 0_usize; //descoffset循环记录
                    for _ in 0..fs_layout.descs_per_block {
                        if let Some(desc) = desc_iter.next() {
                            desc.to_disk_bytes(
                                &mut buffer
                                    [current_offset..current_offset + desc_size as usize],
                            );
                            current_offset += desc_size as usize;
                        }
                    }
                    //写回磁盘
                    block_dev.write_block(gdt_block_id as u32, true)?;
                }
            }
        }
    }

    Ok(())
}

/// 写入块组0的描述符 管字节序
fn write_group_desc<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    group_id: u32,
    desc: &Ext4GroupDesc,
) -> BlockDevResult<()> {
    // 读取超级块以确定块组描述符大小
    let superblock = read_superblock(block_dev)?;
    let desc_size = superblock.get_desc_size() as usize;
    
    // GDT 基地址统一为块号 1 的起始字节偏移：按字节偏移计算所在块和块内偏移
    let gdt_base: u64 = BLOCK_SIZE as u64;
    let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
    let block_size_u64 = BLOCK_SIZE as u64;
    let block_num = byte_offset / block_size_u64;
    let in_block = (byte_offset % block_size_u64) as usize;
    let end = in_block + desc_size;

    // 读取目标块，修改对应 slice，再写回
    block_dev.read_block(block_num as u32)?;
    let buffer = block_dev.buffer_mut();
    if end > buffer.len() {
        return Err(BlockDevError::Corrupted);
    }
    desc.to_disk_bytes(&mut buffer[in_block..end]);
    block_dev.write_block(block_num as u32, true)?;

    Ok(())
}

/// 初始化块组0
fn initialize_group_0<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    layout: &FsLayoutInfo,
) -> BlockDevResult<()> {
    // 计算块组0的布局
    let block_bitmap_blk = layout.group0_block_bitmap;
    let inode_bitmap_blk = layout.group0_inode_bitmap;
    let inode_table_blk = layout.group0_inode_table;

    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
        // 标记元数据块为已使用：块0(引导) + 块1(超级块) + GDT + 块位图 + inode位图 + inode表
        let used_metadata_blocks = layout.group0_metadata_blocks as usize;
        for i in 0..used_metadata_blocks {
            let byte_idx = i / 8;
            let bit_idx = i % 8;
            buffer[byte_idx] |= 1 << bit_idx;
        }
    }
    block_dev.write_block(block_bitmap_blk, true)?;

    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
        // 标记前 RESERVED_INODES 个inode为已使用（保留inode 1-10）
        for i in 0..RESERVED_INODES {
            let byte_idx = (i / 8) as usize;
            let bit_idx = i % 8;
            buffer[byte_idx] |= 1 << bit_idx;
        }

        // 2.5padding无效inode为1
        let bits_per_group = BLOCK_SIZE_U32 * 8;
        for i in layout.inodes_per_group..bits_per_group {
            let byte_idx: usize = (i / 8) as usize;
            let bit_idx = i % 8;
            buffer[byte_idx] |= 1 << bit_idx;
        }
    }
    block_dev.write_block(inode_bitmap_blk, true)?;

    //  清零inode表
    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
    }
    for i in 0..layout.inode_table_blocks {
        block_dev.write_block(inode_table_blk + i, true)?;
    }

    //  更新块组0的描述符（清除UNINIT标志）
    let mut desc = Ext4GroupDesc::default();
    desc.bg_flags = Ext4GroupDesc::EXT4_BG_INODE_ZEROED;
    desc.bg_free_blocks_count_lo = layout
        .blocks_per_group
        .saturating_sub(layout.group0_metadata_blocks) as u16;
    desc.bg_free_inodes_count_lo = layout.inodes_per_group.saturating_sub(RESERVED_INODES) as u16;
    desc.bg_block_bitmap_lo = block_bitmap_blk;
    desc.bg_inode_bitmap_lo = inode_bitmap_blk;
    desc.bg_inode_table_lo = inode_table_blk;

    write_group_desc(block_dev, 0, &desc)?;

    Ok(())
}

/// 初始化除块组0之外的所有块组的位图
/// 对于未使用任何块/ inode 的块组，位图全部清零，free_counts 等于整组容量
fn initialize_other_groups_bitmaps<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    layout: &FsLayoutInfo,
    sb: &Ext4Superblock,
) -> BlockDevResult<()> {
    // 从块组1开始，逐组初始化
    for group_id in 1..layout.groups {
        // 使用与 build_uninit_group_desc 相同的布局计算
        let gl = cloc_group_layout(
            group_id,
            sb,
            layout.blocks_per_group,
            layout.inode_table_blocks,
            layout.group0_block_bitmap,
            layout.group0_inode_bitmap,
            layout.group0_inode_table,
            layout.gdt_blocks,
        );

        let block_bitmap_blk = gl.group_blcok_bitmap_startblocks as u32;
        let inode_bitmap_blk = gl.group_inode_bitmap_startblocks as u32;

        //  初始化块位图：全0 → 所有块空闲
        {
            let buffer = block_dev.buffer_mut();
            buffer.fill(0);
            // 标记元数据块已用（包括备份 superblock/GDT、位图和 inode 表）
            let used_blocks = gl.metadata_blocks_in_group as usize;
            for i in 0..used_blocks {
                let byte_idx = i / 8;
                let bit_idx = i % 8;
                buffer[byte_idx] |= 1 << bit_idx;
            }
        }
        block_dev.write_block(block_bitmap_blk, true)?;

        {
            //  初始化inode位图：全0 → 所有inode空闲
            let buffer = block_dev.buffer_mut();
            buffer.fill(0);

            // padding无效inode
            let bits_per_group = BLOCK_SIZE_U32 * 8;
            for i in layout.inodes_per_group..bits_per_group {
                let byte_idx: usize = (i / 8) as usize;
                let bit_idx = i % 8;
                buffer[byte_idx] |= 1 << bit_idx;
            }
        }
        block_dev.write_block(inode_bitmap_blk, true)?;
    }

    Ok(())
}
