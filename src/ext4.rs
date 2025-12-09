//! Ext4文件系统主调用入口
//!
//! 提供文件系统挂载、卸载、文件操作等高层接口

use crate::blockdev::{BlockDev, BlockDevice, BlockDevError, BlockDevResult};
use crate::datablock_cache::DataBlockCache;
use crate::endian::DiskFormat;
use crate::jbd2::jbd2::{create_journal_entry, dump_journal_inode};
use crate::entries::Ext4DirEntry2;
use crate::inodetable_cache::InodeCache;
use crate::loopfile::get_file_inode;
use crate::mkd::{create_lost_found_directory, create_root_directory_entry, get_inode_with_num, mkdir};
use crate::mkfile::mkfile;
use crate::superblock::Ext4Superblock;
use crate::blockgroup_description::Ext4GroupDesc;
use crate::bmalloc::{BlockAllocator, InodeAllocator};
use crate::bitmap_cache::{BitmapCache, CacheKey};
use crate::config::*;
use crate::tool::{cloc_group_layout, debugSuperAndDesc, need_redundant_backup};
use alloc::collections::vec_deque::VecDeque;
use log::{debug, error, info, warn};
use alloc::vec::Vec;
use alloc::string::{String, ToString};
use crate::disknode::Ext4Inode;
use crate::BlockDevError::BufferTooSmall;
/// Ext4文件系统挂载错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountError {
    /// IO错误
    IoError,
    /// 魔数无效
    InvalidMagic,
    /// 超级块无效（如GDT超出预留空间）
    InvalidSuperblock,
    /// 文件系统有错误
    FilesystemHasErrors,
    /// 不支持的特性
    UnsupportedFeature,
    /// 已经挂载
    AlreadyMounted,
}


impl core::fmt::Display for MountError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MountError::IoError => write!(f, "IO错误"),
            MountError::InvalidMagic => write!(f, "魔数无效"),
            MountError::InvalidSuperblock => write!(f, "超级块无效"),
            MountError::FilesystemHasErrors => write!(f, "文件系统有错误"),
            MountError::UnsupportedFeature => write!(f, "不支持的特性"),
            MountError::AlreadyMounted => write!(f, "文件系统已挂载"),
        }
    }
}

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
}

impl Ext4FileSystem {
    //目录是否存在
     pub fn file_entries_exist<B: BlockDevice>(&mut self,device:&mut BlockDev<B>,path:&str)->bool{
        let inode = get_file_inode(self, device, path).expect("no dir");
        match &inode {
            Some(inode)=>{
                debug!("Find it! Inode:{:?}",&inode);
                true
            }
            None=>{
                warn!("Not Find it:(");
                false
            }
        }
    }

    ///遍历目录
    pub fn find_file<B: BlockDevice>(&mut self,device:&mut BlockDev<B>,path:&str)->Option<Ext4Inode>{
        let inode = get_file_inode(self, device, path).expect("no dir");
        match &inode {
            Some(inode)=>{
                debug!("Found it: {} !",path);
                Some(inode.clone())
            }
            None=>{
                warn!("Not found: {} !",path);
                None
            }
        }
    }


    ///获取根目录
    ///上层api封装 获取根目录 inode为2
    pub fn get_root<B: BlockDevice>(&mut self, block_dev: &mut BlockDev<B>) -> BlockDevResult<Ext4Inode> {
        let root_inode_num = self.root_inode as u64;
        // 根目录位于块组0的 inode 表中，起始块号从块组描述符读取
        let inode_table_start = match self.group_descs.get(0) {
            Some(desc) => desc.inode_table() as u64,
            None => return Err(BlockDevError::Corrupted),
        };
        let (block_num, offset, _group_idx) = self.inodetable_cahce.calc_inode_location(
            self.root_inode,
            self.superblock.s_inodes_per_group,
            inode_table_start,
            BLOCK_SIZE,
        );
        let result = self
            .inodetable_cahce
            .get_or_load(block_dev, root_inode_num, block_num, offset)?;
        debug!("Root inode i_mode: {}", result.inode.i_mode);
        debug!("Root inode detail: {:?}", result.inode);
        Ok(result.inode)
    }

    ///创建lost+found
    fn create_lost_found_dir<B: BlockDevice>(&mut self, block_dev: &mut BlockDev<B>) -> BlockDevResult<()> {
        // 逻辑迁移到 mkd::create_root_directory_entry 中
        create_lost_found_directory(self, block_dev)
    }
    ///创建根目录
    ///文件系统初始化时调用
    fn create_root_dir<B: BlockDevice>(&mut self, block_dev: &mut BlockDev<B>) -> BlockDevResult<()> {
        // 逻辑迁移到 mkd::create_root_directory_entry 中
        create_root_directory_entry(self, block_dev)
    }

    /// 挂载Ext4文件系统
    pub fn mount<B: BlockDevice>(block_dev: &mut BlockDev<B>) -> Result<Self, MountError> {
        debug!("Start mounting Ext4 filesystem...");
        
        // 1. 读取超级块（按 ext4 标准偏移 1024 字节，大小 1024 字节）
        let superblock = read_superblock(block_dev).map_err(|_| MountError::IoError)?;
        debug!("Superblock: {:?}", &superblock);
        
        // 2. 验证魔数
        if superblock.s_magic != EXT4_SUPER_MAGIC {
            error!("Invalid magic: {:#x}, expected: {:#x}", 
                   superblock.s_magic, EXT4_SUPER_MAGIC);
            return Err(MountError::InvalidMagic);
        }
        debug!("Superblock magic verified");
        
        // 3. 检查文件系统状态
        if superblock.s_state == Ext4Superblock::EXT4_ERROR_FS {
            warn!("Filesystem is in error state");
            return Err(MountError::FilesystemHasErrors);
        }
        
        // 4. 计算块组数量
        let group_count = superblock.block_groups_count();
        debug!("Block group count: {}", group_count);

        // 5. 读取所有块组描述符
        let group_descs = Self::load_group_descriptors(
            block_dev, 
            group_count, 
            superblock.s_desc_size as usize
        )?;
        debug!("Loaded {} group descriptors", group_descs.len());
        
        // 6. 初始化分配器
        let block_allocator = BlockAllocator::new(&superblock);
        let inode_allocator = InodeAllocator::new(&superblock);
        debug!("Allocators initialized");
        
        // 7. 初始化位图缓存（最多缓存8个位图）
        let bitmap_cache = BitmapCache::default();
        debug!("Bitmap cache initialized (lazy loading)");

        // 初始化inode缓存
        let inode_cache = InodeCache::new(INODE_CACHE_MAX, INODE_SIZE as usize);
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
        };

        //创建journal 注意，目前不能进行挂载，由于测试，journal的数据块只有1 经过测试内核会拒绝挂载!
        create_journal_entry(&mut fs,block_dev);
        //dump_journal_inode(&mut fs, block_dev);


         //详细debug输出
        debugSuperAndDesc(&fs.superblock, &fs);

        // rootinode check !
        debug!("Checking root directory...");
        {
            let root_inode = fs
                .get_root(block_dev)
                .map_err(|_| MountError::IoError)?;
            if root_inode.i_mode == 0 || !root_inode.is_dir() {
                warn!("Root inode is uninitialized or not a directory, creating root and lost+found... i_mode: {}, is_dir: {}", root_inode.i_mode, root_inode.is_dir());
                fs.create_root_dir(block_dev)
                    .map_err(|_| MountError::IoError)?;
            }
        }

        // lost+found check!
        debug!("Checking lost+found directory...");
        {
            // 1. 优先信任超级块中的 s_lpf_ino（如果非 0）
            if fs.superblock.s_lpf_ino != 0 {
                let ino = fs.superblock.s_lpf_ino;
                debug!("Lost+found inode recorded in superblock: {}", ino);
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

        //详细的Inode/DataBlock占用情况
        {
            let g0 = match fs.group_descs.get(0) {
                Some(desc) => desc,
                None => return Err(MountError::InvalidSuperblock),
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
                "Bitmap usage: inodes used = {}, data blocks used = {}",
                indoe_count, datablock_count
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
        fs.bitmap_cache.flush_all(block_dev).expect("flush failed!");
        fs.inodetable_cahce.flush_all(block_dev).expect("flush failed!");

        Ok(fs)
    }
    
    /// 加载所有块组描述符 顺序性
    fn load_group_descriptors<B: BlockDevice>(
        block_dev: &mut BlockDev<B>,
        group_count: u32,
        desc_size: usize,
    ) -> Result<Vec<Ext4GroupDesc>, MountError> {
        let mut group_descs = Vec::new();

       
        let gdt_base: u64 = BLOCK_SIZE as u64;

        // 为了减少重复读块，这里缓存当前块号
        let mut current_block: Option<u64> = None;

        debug!("Loading group descriptors: {} groups, desc_size = {} bytes", group_count, desc_size);
        for group_id in 0..group_count {
            let byte_offset = gdt_base + group_id as u64 * desc_size as u64;
            let block_size_u64 = BLOCK_SIZE as u64;
            let block_num = byte_offset / block_size_u64;
            let in_block = (byte_offset % block_size_u64) as usize;

            // 只在块号变化时重新读取块
            if current_block != Some(block_num) {
                block_dev
                    .read_block(block_num as u32)
                    .map_err(|_| MountError::IoError)?;
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
                return Err(MountError::InvalidSuperblock);
            }

            let desc = Ext4GroupDesc::from_disk_bytes(&buffer[in_block..end]);
            group_descs.push(desc);
        }

        debug!("Successfully loaded {} group descriptors", group_descs.len());
        Ok(group_descs)
    }
    
    /// 卸载文件系统 不写超级块备份
    pub fn umount<B: BlockDevice>(&mut self, block_dev: &mut BlockDev<B>) -> BlockDevResult<()> {
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
        
        //同步group_desc 和 super_block计数
        let mut real_free_blocks :u64=0;
        let mut real_free_inodes:u64=0;
        for desc in &self.group_descs {
            real_free_blocks+=desc.free_blocks_count() as u64;
            real_free_inodes+=desc.free_inodes_count() as u64;
        }
        self.superblock.s_free_blocks_count_lo = (real_free_blocks & 0xFFFFFFFF) as u32;
        self.superblock.s_free_blocks_count_hi = (real_free_blocks >> 32) as u32;
        self.superblock.s_free_inodes_count = real_free_inodes as u32;

        // 4. Update superblock
        info!("Writing back superblock...");
        self.sync_superblock(block_dev)?;
        debug!("Superblock updated");
        
        // Write back group descriptors
        debug!("Writing back group descriptors...");
        self.sync_group_descriptors(block_dev)?;
        
        self.mounted = false;
        info!("Filesystem unmounted cleanly");
        Ok(())
    }
    
    /// 同步块组描述符到磁盘
    /// 按 ext4 标准布局，将所有块组描述符写回：
    /// GDT 字节流紧跟在超级块之后
    fn sync_group_descriptors<B: BlockDevice>(
        &self,
        block_dev: &mut BlockDev<B>,
    ) -> BlockDevResult<()> {
        let total_desc_count = self.group_descs.len();
        let desc_size = GROUP_DESC_SIZE as usize;

        // GDT 基地址统一为块号 1 的起始字节偏移
        let gdt_base: u64 = BLOCK_SIZE as u64;
        let block_size_u64 = BLOCK_SIZE as u64;

        debug!(
            "Writing back group descriptors: {} descriptors, desc_size = {} bytes",
            total_desc_count,
            desc_size
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
                if let Some(prev_block) = current_block {
                    if Some(prev_block) == buffer_snapshot_block {
                        block_dev.write_block(prev_block as u32)?;
                    }
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
        if let Some(last_block) = current_block {
            if Some(last_block) == buffer_snapshot_block {
                block_dev.write_block(last_block as u32)?;
            }
        }

        debug!("Group descriptors written back");
        Ok(())
    }

    /// 同时修改所有需要冗余备份的块组
    /// 同步超级块到磁盘
    fn sync_superblock<B: BlockDevice>(&self, block_dev: &mut BlockDev<B>) -> BlockDevResult<()> {
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
    
    /// 在指定块组分配一个块 应该管理GROUPDESC
    /// 
    /// # 参数
    /// * `block_dev` - 块设备
    /// * `group_idx` - 块组索引
    /// 
    /// # 返回
    /// 成功返回全局块号
    pub fn alloc_block<B: BlockDevice>(
        &mut self,
        block_dev: &mut BlockDev<B>,
        group_idx: u32,
    ) -> BlockDevResult<u64> {
        use crate::bmalloc::BlockAlloc;
        
        // 1. 预先复制group_desc（避免借用冲突）
        let (bitmap_block, group_desc_copy) = {
            let group_desc = self.get_group_desc(group_idx)
                .ok_or(BlockDevError::Corrupted)?;
            
            if group_desc.free_blocks_count() == 0 {
                return Err(BlockDevError::NoSpace);
            }
            
            (group_desc.block_bitmap(), *group_desc)  // 复制整个结构体
        };
        
        // 2. 使用位图缓存的 modify 接口分配块并标记脏页
        let cache_key = CacheKey::new_block(group_idx);
        let mut alloc_res: Result<BlockAlloc, BlockDevError> = Err(BlockDevError::NoSpace);
        self.bitmap_cache
            .modify(block_dev, cache_key, bitmap_block, |data| {
                let r = self
                    .block_allocator
                    .alloc_block_in_group(data, group_idx, &group_desc_copy);
                alloc_res = r.map_err(|_| BlockDevError::NoSpace);
            })?;

        let alloc = alloc_res?;
        
        // 5. 更新块组描述符
        if let Some(desc) = self.get_group_desc_mut(group_idx) {
            let new_count = desc.free_blocks_count().saturating_sub(1);
            desc.bg_free_blocks_count_lo = (new_count & 0xFFFF) as u16;
            desc.bg_free_blocks_count_hi = (new_count >> 16) as u16;
        }
        
        // 6. 更新超级块
        self.superblock.s_free_blocks_count_lo = 
            self.superblock.s_free_blocks_count_lo.saturating_sub(1);
        
        debug!("Allocated block: group={}, block_in_group={}, global_block={} [delayed write]", 
               alloc.group_idx, alloc.block_in_group, alloc.global_block);
        
        Ok(alloc.global_block)
    }
    
    /// 在指定块组分配一个inode
    /// 
    /// # 参数
    /// * `block_dev` - 块设备
    /// * `group_idx` - 块组索引
    /// 
    /// # 返回
    /// 成功返回全局inode号
    pub fn alloc_inode<B: BlockDevice>(
        &mut self,
        block_dev: &mut BlockDev<B>,
        group_idx: u32,
    ) -> BlockDevResult<u32> {
        use crate::bmalloc::InodeAlloc;
        
        // 1. 预先复制group_desc（避免借用冲突）
        let (bitmap_block, group_desc_copy) = {
            let group_desc = self.get_group_desc(group_idx)
                .ok_or(BlockDevError::Corrupted)?;
            
            if group_desc.free_inodes_count() == 0 {
                return Err(BlockDevError::NoSpace);
            }
            
            (group_desc.inode_bitmap(), *group_desc)  // 复制整个结构体
        };
        
        // 2. 使用位图缓存的 modify 接口分配 inode 并标记脏页
        let cache_key = CacheKey::new_inode(group_idx);
        let mut alloc_res: Result<crate::bmalloc::InodeAlloc, BlockDevError> =
            Err(BlockDevError::NoSpace);
        self.bitmap_cache
            .modify(block_dev, cache_key, bitmap_block, |data| {
                let r = self
                    .inode_allocator
                    .alloc_inode_in_group(data, group_idx, &group_desc_copy);
                alloc_res = r.map_err(|_| BlockDevError::NoSpace);
            })?;

        let alloc = alloc_res?;
        
        // 6. 更新块组描述符
        if let Some(desc) = self.get_group_desc_mut(group_idx) {
            let new_count = desc.free_inodes_count().saturating_sub(1);
            desc.bg_free_inodes_count_lo = (new_count & 0xFFFF) as u16;
            desc.bg_free_inodes_count_hi = (new_count >> 16) as u16;
            
        }
        
        // 7. 更新超级块
        self.superblock.s_free_inodes_count = 
            self.superblock.s_free_inodes_count.saturating_sub(1);
        
        debug!("Allocated inode: group={}, inode_in_group={}, global_inode={} [delayed write]", 
               alloc.group_idx, alloc.inode_in_group, alloc.global_inode);
        
        Ok(alloc.global_inode)
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
    pub fn make_base_dir(&self){
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
pub fn file_entry_exisr<B: BlockDevice>(fs:&mut Ext4FileSystem,device:&mut BlockDev<B>,path:&str)->bool{
    fs.file_entries_exist(device, path)
}
/// 文件寻找函数-线性扫描
pub fn find_file<B: BlockDevice>(fs:&mut Ext4FileSystem,device:&mut BlockDev<B>,path:&str)->Option<Ext4Inode>{
    fs.find_file(device, path)
}

/// 简化的挂载函数（用于兼容旧代码）
pub fn mount<B: BlockDevice>(block_dev: &mut BlockDev<B>) -> BlockDevResult<Ext4FileSystem> {
    match Ext4FileSystem::mount(block_dev) {
        Ok(_fs) => {
            info!("Ext4 filesystem mounted");
            Ok(_fs)
        }
        Err(e) => {
            error!("Mount failed: {}", e);
            Err(BlockDevError::Corrupted)
        }
    }
}

///取消挂载函数
pub fn umount<B: BlockDevice>(fs:Ext4FileSystem,block_dev: &mut BlockDev<B>) -> BlockDevResult<()> {
    let mut f= fs;
    f.umount(block_dev)?;
    Ok(())
}

/// 文件系统布局信息（仅用于 mkfs 阶段的计算）
struct FsLayoutInfo {
    /// 逻辑块大小（字节）
    block_size: u32,
    /// 每组块数
    blocks_per_group: u32,
    /// 每组 inode 数
    inodes_per_group: u32,
    /// inode 大小（字节）
    inode_size: u16,
    /// 总块数
    total_blocks: u64,
    /// 块组数
    groups: u32,
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

pub fn compute_fs_layout(total_blocks:u64)->FsLayoutInfo{
    let block_size: u32 = 1024u32 << LOG_BLOCK_SIZE;

    // 每组块数：8 * block_size（标准 ext4 默认）
    let blocks_per_group: u32 = 8 * block_size;

    // 每组 inode 数：blocks_per_group / 4（简化策略）
    let inodes_per_group: u32 = blocks_per_group / 4;

    let inode_size: u16 = INODE_SIZE;

    // 块组数：向上取整
    let groups: u32 = ((total_blocks + blocks_per_group as u64 - 1) / blocks_per_group as u64) as u32;

    // 每块能容纳的组描述符个数
    let descs_per_block: u32 = if GROUP_DESC_SIZE == 0 {
        0
    } else {
        block_size / GROUP_DESC_SIZE as u32
    };

    // GDT 实际占用的块数
    let gdt_blocks: u32 = if descs_per_block == 0 {
        0
    } else {
        (groups + descs_per_block - 1) / descs_per_block
    };

    // 每组 inode 表占用的块数
    let inode_table_blocks: u32 = if block_size == 0 {
        0
    } else {
        (inodes_per_group * inode_size as u32 + block_size - 1) / block_size
    };

    // 第一个数据块：块大小 > 1024 时为 0，否则为 1（参考 lwext4 create_fs_aux_info）
    let first_data_block: u32 = if block_size > 1024 { 0 } else { 1 };

    // 预留的 GDT 块数（与 ext4 标准一致）
    let reserved_gdt_blocks: u32 = RESERVED_GDT_BLOCKS as u32;

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
        total_blocks,
        groups,
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


   

pub fn mkfs<B: BlockDevice>(block_dev: &mut BlockDev<B>) -> BlockDevResult<()> {
    debug!("Start initializing Ext4 filesystem...");
    
    // 1. 计算布局参数
    let total_blocks = block_dev.total_blocks();
    let layout = compute_fs_layout(total_blocks);
    let total_groups = layout.groups;
    
    debug!("  Total blocks: {}", total_blocks);
    debug!("  Block size: {} bytes", layout.block_size);
    debug!("  Block group count: {}", total_groups);
    debug!("  Blocks per group: {}", layout.blocks_per_group);
    debug!("  Inodes per group: {}", layout.inodes_per_group);
    
    //构建并根据fearure写入到所有group超级块
    let superblock = build_superblock(
        total_blocks,
        &layout,
    );
    write_superblock(block_dev, &superblock)?;
    debug!("Superblock written");

    //写冗余备份 自动判断是否写
    write_superblock_redundant_backup(block_dev, &superblock, total_groups, &layout)?;

    //注意顺序
    let mut descs:VecDeque<Ext4GroupDesc>=VecDeque::new();
    //为superblock写入gdt（全部标记为UNINIT）
    for group_id in 0..total_groups {
        let desc = build_uninit_group_desc(&superblock,group_id, &layout);
        write_group_desc(block_dev, group_id, &desc)?;
        descs.push_back(desc);
    }
    //为其它块组选择性的写入冗余备份desc
    write_gdt_redundant_backup(block_dev,  &descs, &superblock, total_groups, &layout)?;
    debug!("{} block group descriptors written", total_groups);
    
    //实际初始化块组0（用于根目录）
    initialize_group_0(block_dev, &layout)?;
    debug!("Block group 0 initialized (for root directory)");
    
    // 初始化其它块组的位图（全部视为空闲）
    initialize_other_groups_bitmaps(block_dev, &layout,&superblock)?;

    //通过一次挂载/卸载流程，让根目录在 mkfs 阶段就被真正创建并写回磁盘
    {
        let mut fs = Ext4FileSystem::mount(block_dev).expect("Mount Failed!");
        // mount 内部如果发现 root inode 未初始化，会调用 create_root_dir
        fs.umount(block_dev)?;
    }


    //  验证：读回超级块检查魔数
    let verify_sb = read_superblock(block_dev)?;
    if verify_sb.s_magic == EXT4_SUPER_MAGIC {
        debug!("Format completed, superblock magic verified: {:#x}", verify_sb.s_magic);
        Ok(())
    } else {
        debug!("Superblock magic verification failed");
        Err(BlockDevError::Corrupted)
    }
}



/// 构建超级块 不管字节序
fn build_superblock(
    total_blocks: u64,
    layout: &FsLayoutInfo,
) -> Ext4Superblock {
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


    // 空闲计数：总块数 - 组0元数据块数 - 预留块数（其余组初始全空闲）
    let metadata_blocks = layout.group0_metadata_blocks as u64;
    let mut free_blocks = total_blocks
        .saturating_sub(metadata_blocks)
        .saturating_sub(layout.reserved_blocks);
    if free_blocks > total_blocks { free_blocks = 0; }
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
    sb.s_desc_size = GROUP_DESC_SIZE;

    // 预留的 GDT 块数（仅 mkfs 默认值，挂载时应相信磁盘中的值）
    sb.s_reserved_gdt_blocks = layout.reserved_gdt_blocks as u16;

    sb
}

/// 构建未初始化的块组描述符 不管字节序 
fn build_uninit_group_desc(sb:&Ext4Superblock,group_id: u32, layout: &FsLayoutInfo) -> Ext4GroupDesc {
    let mut desc = Ext4GroupDesc::default();

    // 通过工具函数统一计算该块组的布局
    let gl = crate::tool::cloc_group_layout(
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
    let free_blocks = layout
        .blocks_per_group
        .saturating_sub(used_meta);

    if group_id == 0 {
        // 组0 还需要扣掉保留 inode
        desc.bg_free_blocks_count_lo = free_blocks as u16;
        desc.bg_free_inodes_count_lo = layout
            .inodes_per_group
            .saturating_sub(RESERVED_INODES) as u16;
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
    block_dev: &mut BlockDev<B>,
    sb: &Ext4Superblock,
    groups_count:u32,
    fs_layout:&FsLayoutInfo,
) -> BlockDevResult<()> {

    //从1开始
    // sparse_superbllock特性判断
    let sprse_feature = sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sprse_feature {
        for gid in 1..groups_count {
            let group_layout = cloc_group_layout(gid, sb, fs_layout.blocks_per_group, 
                fs_layout.inode_table_blocks, fs_layout.group0_block_bitmap, fs_layout.group0_inode_bitmap,
                fs_layout.group0_inode_table, fs_layout.gdt_blocks);
            //需要超级块备份
            if need_redundant_backup(gid){
                let super_blocks = group_layout.group_start_block;
                block_dev.read_block(super_blocks as u32);
                let buffer = block_dev.buffer_mut();
                sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
                block_dev.write_block(super_blocks as u32)?;
            }
        }
    }
    Ok(())
}



/// 写入超级块到磁盘 管字节序 不写备份
fn write_superblock<B: BlockDevice>(
    block_dev: &mut BlockDev<B>,
    sb: &Ext4Superblock,
) -> BlockDevResult<()> {
    // 超级块总是从分区偏移 1024 字节开始，占用 1024 字节
    if BLOCK_SIZE == 1024 {
        block_dev.read_block(1)?;
        let buffer = block_dev.buffer_mut();
        sb.to_disk_bytes(&mut buffer[0..SUPERBLOCK_SIZE]);
        block_dev.write_block(1)?;
    } else {
        block_dev.read_block(0)?;
        let buffer = block_dev.buffer_mut();
        let offset = Ext4Superblock::SUPERBLOCK_OFFSET as usize; // 1024
        let end = offset + Ext4Superblock::SUPERBLOCK_SIZE;
        sb.to_disk_bytes(&mut buffer[offset..end]);
        block_dev.write_block(0)?;
    }


    Ok(())
}

/// 读取超级块 管字节序
fn read_superblock<B: BlockDevice>(
    block_dev: &mut BlockDev<B>,
) -> BlockDevResult<Ext4Superblock> {
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
    block_dev: &mut BlockDev<B>,
    descs: &VecDeque<Ext4GroupDesc>,
    sb: &Ext4Superblock,
    groups_count:u32,
    fs_layout:&FsLayoutInfo
) -> BlockDevResult<()> {
    //参数合法性判断
    let desc_all_size = descs.len()*GROUP_DESC_SIZE as usize;
    let can_recive_size = fs_layout.gdt_blocks * fs_layout.descs_per_block * GROUP_DESC_SIZE as u32;
    if can_recive_size < desc_all_size as u32 {
       return Err(BlockDevError::BufferTooSmall { provided: can_recive_size as usize, required: desc_all_size });
    }


    let sprse_feature = sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER);
    if sprse_feature {
        //为每个块组执行
        for gid in 1..groups_count {
            if need_redundant_backup(gid) {
                let group_layout = cloc_group_layout(gid, sb, fs_layout.blocks_per_group, 
                fs_layout.inode_table_blocks, fs_layout.group0_block_bitmap, fs_layout.group0_inode_bitmap,
                fs_layout.group0_inode_table, fs_layout.gdt_blocks);
                let gdt_start = group_layout.group_start_block+1;//跳过超级块


                let mut desc_iter = descs.iter();
                //循环写入desc
                for gdt_block_id in gdt_start..group_layout.group_blcok_bitmap_startblocks {
                    block_dev.read_block(gdt_block_id as u32)?;
                    let buffer = block_dev.buffer_mut();
                    let mut current_offset = 0_usize;//descoffset循环记录
                    for _ in 0..fs_layout.descs_per_block {
                        if let Some(desc) = desc_iter.next(){
                            desc.to_disk_bytes(&mut buffer[current_offset..current_offset+GROUP_DESC_SIZE as usize]);
                            current_offset+=GROUP_DESC_SIZE as usize;
                        }
                    }
                    //写回磁盘
                    block_dev.write_block(gdt_block_id as u32)?;
                }
            }
        }
    }



    Ok(())

}


/// 写入块组0的描述符 管字节序
fn write_group_desc<B: BlockDevice>(
    block_dev: &mut BlockDev<B>,
    group_id: u32,
    desc: &Ext4GroupDesc,
) -> BlockDevResult<()> {
    // GDT 基地址统一为块号 1 的起始字节偏移：按字节偏移计算所在块和块内偏移
    let desc_size = GROUP_DESC_SIZE as usize;
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
    block_dev.write_block(block_num as u32)?;

    Ok(())
}

/// 初始化块组0
fn initialize_group_0<B: BlockDevice>(
    block_dev: &mut BlockDev<B>,
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
    block_dev.write_block(block_bitmap_blk)?;
    
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
        let bits_per_group =BLOCK_SIZE_U32*8;
        for i in layout.inodes_per_group..bits_per_group {
            let byte_idx :usize= (i/8) as usize;
            let bit_idx = i%8;
            buffer[byte_idx] |=1<<bit_idx;
        } 
    }
    block_dev.write_block(inode_bitmap_blk)?;


    
    //  清零inode表
    {
        let buffer = block_dev.buffer_mut();
        buffer.fill(0);
    }
    for i in 0..layout.inode_table_blocks {
        block_dev.write_block(inode_table_blk + i)?;
    }
    
    //  更新块组0的描述符（清除UNINIT标志）
    let mut desc = Ext4GroupDesc::default();
    desc.bg_flags = Ext4GroupDesc::EXT4_BG_INODE_ZEROED;
    desc.bg_free_blocks_count_lo = layout.blocks_per_group
        .saturating_sub(layout.group0_metadata_blocks) as u16;
    desc.bg_free_inodes_count_lo = layout.inodes_per_group
        .saturating_sub(RESERVED_INODES) as u16;
    desc.bg_block_bitmap_lo = block_bitmap_blk;
    desc.bg_inode_bitmap_lo = inode_bitmap_blk;
    desc.bg_inode_table_lo = inode_table_blk;
    
    write_group_desc(block_dev, 0, &desc)?;
    
    Ok(())
}

/// 初始化除块组0之外的所有块组的位图
/// 对于未使用任何块/ inode 的块组，位图全部清零，free_counts 等于整组容量
fn initialize_other_groups_bitmaps<B: BlockDevice>(
    block_dev: &mut BlockDev<B>,
    layout: &FsLayoutInfo,
    sb:&Ext4Superblock
) -> BlockDevResult<()> {
    // 从块组1开始，逐组初始化
    for group_id in 1..layout.groups {
        // 使用与 build_uninit_group_desc 相同的布局计算
        let gl = crate::tool::cloc_group_layout(
            group_id,
            &sb,
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
        block_dev.write_block(block_bitmap_blk)?;

        {
            //  初始化inode位图：全0 → 所有inode空闲
            let buffer = block_dev.buffer_mut();
            buffer.fill(0);

            // padding无效inode
            let bits_per_group =BLOCK_SIZE_U32*8;
            for i in layout.inodes_per_group..bits_per_group {
                let byte_idx :usize= (i/8) as usize;
                let bit_idx = i%8;
                buffer[byte_idx] |=1<<bit_idx;
            } 

        }
        block_dev.write_block(inode_bitmap_blk)?;

    }

    Ok(())
}