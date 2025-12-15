use alloc::vec::Vec;
use log::{error, trace, warn};

use crate::ext4_backend::config::*;
use crate::ext4_backend::jbd2::jbdstruct::*;
use crate::ext4_backend::error::*;



///可以调用block write的函数标记 有序管理写,jbd2需要
pub trait INeedBlockdevToWrite {}

/// 外部需要实现的块设备trait
pub trait BlockDevice {
    /// 写入数据到块设备
    /// * `buffer` - 要写入的数据
    /// * `block_id` - 起始块号
    /// * `count` - 块数量
    fn write(&mut self, buffer: &[u8], block_id: u32, count: u32) -> BlockDevResult<()>;

    /// 从块设备读取数据
    /// * `buffer` - 读取数据的目标缓冲区
    /// * `block_id` - 起始块号
    /// * `count` - 块数量
    fn read(&mut self, buffer: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()>;

    /// 打开块设备
    fn open(&mut self) -> BlockDevResult<()>;

    /// 关闭块设备
    fn close(&mut self) -> BlockDevResult<()>;

    /// 获取块设备的总块数
    fn total_blocks(&self) -> u64;

    /// 获取块大小（字节）
    fn block_size(&self) -> u32 {
        512 // 默认512字节
    }

    /// 刷新缓存到磁盘
    fn flush(&mut self) -> BlockDevResult<()> {
        Ok(()) // 默认实现为空操作
    }

    /// 检查设备是否已打开
    fn is_open(&self) -> bool {
        true // 默认认为已打开
    }

    /// 检查设备是否只读
    fn is_readonly(&self) -> bool {
        false // 默认为可读写
    }
}

/// 块设备缓存
pub struct BlockBuffer {
    buffer: [u8; BLOCK_SIZE],
}

impl BlockBuffer {
    /// 创建新的块缓冲区
    pub fn new() -> Self {
        Self {
            buffer: [0u8; BLOCK_SIZE],
        }
    }

    /// 获取缓冲区引用
    pub fn as_slice(&self) -> &[u8] {
        &self.buffer
    }

    /// 获取可变缓冲区引用
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    /// 获取缓冲区大小
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// 清空缓冲区
    pub fn clear(&mut self) {
        self.buffer.fill(0);
    }
}

impl Default for BlockBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// 块设备封装
/// 提供缓存和便捷的块设备操作接口
struct BlockDev<B: BlockDevice> {
    dev: B,
    buffer: BlockBuffer,
    is_dirty: bool,            // 缓冲区是否已修改
    cached_block: Option<u32>, // 当前缓存的块号
}
pub enum Jbd2RunState {
    Commit,
    Replay,
}
pub struct Jbd2Dev<B: BlockDevice> {
    _mode: u8, //日志级别，默认ordered 0
    inner: BlockDev<B>,
    journal_use: bool, //是否启用日志系统
    _state: Jbd2RunState,
    systeam: Option<JBD2DEVSYSTEM>,
}

///jbd2代理blockdev
///只记录metadata
/// 采用Jouranl超级快注入的思想，必须需要使用mount来给块设备注入超级块，之后才能使用日志。
impl<B: BlockDevice> Jbd2Dev<B> {
    ///你拿到我之后应该先把超级块给我传进来吧
    pub fn initial_jbd2dev(_mode: u8, block_dev:B, use_journal: bool) -> Self {
        let block_dev = BlockDev::new(block_dev);
        Self {
            _mode,
            inner: block_dev,
            journal_use: use_journal,
            _state: Jbd2RunState::Commit,
            systeam: None,
        }
    }

    pub fn is_use_journal(&self) -> bool {
        self.journal_use
    }

    ///外部重放journal日志入口 注意性能影响
    pub fn journal_replay(&mut self) {
        if self.journal_use {
            let dev = &mut self.inner.dev;
            let jbd_sys = &mut self
                .systeam
                .as_mut()
                .expect("jbd2dev are not initial,please initial the jbd2dev first!");
            jbd_sys.replay(&mut *dev);
        } else {
            warn!("Jouranl function not turn ,please turn on this function and retry!");
        }
    }

    /// 运行时打开/关闭日志功能（例如 mkfs 阶段强制关闭，真正挂载再打开）
    pub fn set_journal_use(&mut self, use_journal: bool) {
        self.journal_use = use_journal;
    }

    /// 提前把 journal 超级块塞进来，后续第一次需要用到时再 lazy-init JBD2DEVSYSTEM
    /// 初始化SYSTEAM
    pub fn set_journal_superblock(
        &mut self,
        super_block: JournalSuperBllockS,
        jouranl_start_block: u32,
    ) {
        let system = JBD2DEVSYSTEM {
            start_block: jouranl_start_block,
            max_len: super_block.s_maxlen,
            head: 0,
            sequence: super_block.s_sequence,
            jbd2_super_block: super_block,
            commit_queue: Vec::new(),
        };
        self.systeam = Some(system);
    }

    ///防止滥用，仅仅umount调用，确保事务缓存全部提交完毕
    pub fn umount_commit(&mut self) {
        if self.journal_use {
            self.systeam
                .as_mut()
                .unwrap()
                .commit_transaction(&mut self.inner.dev).expect("Translation commit failed!!!");
        } else {
            warn!("Jouranl not use , no thing to commit")
        }
    }

    pub fn write_block(&mut self, block_id: u32, is_metadata: bool) -> BlockDevResult<()> {
        //error!("write block :{} ,use journal?:{} ismetadata:{}",block_id,self.journal_use,is_metadata);

        // 1) 非元数据 或 未开启日志：直接写回到底层块设备
        if !self.journal_use || !is_metadata {
            // BlockDev 内部的 buffer 已经被上层写好，直接把当前 buffer 写到 block_id
            return self.inner.write_block(block_id);
        }

        // 2) 元数据且启用日志：走 JBD2 事务
        //    此时之前的普通数据块已经完成写入
        //由于分布提交机制，必须需要拷贝数据牺牲性能来确保日志提交
        let meta_vec = self.inner.buffer();
        let updates = Jbd2Update(
            block_id as u64,
            meta_vec
                .try_into()
                .expect("Data can;t into [u8;BLOCK_SIZE] panic!,os should process"),
        );

        if self.systeam.is_none() {
            // 日志标志已开但还没有 journal superblock，暂时按非日志写处理
            error!(
                "Journal systeam uninitial,but journal has turned，this sentence must be once!!!"
            );
            return self.inner.write_block(block_id);
        }

        let systeam = self.systeam.as_mut().unwrap();

        // 使用原始底层块设备提交事务
        let raw_dev = self.inner.device_mut();

        //先写入缓存
        if systeam.commit_queue.len() > JBD2_BUFFER_MAX {
            //缓存已满 直接提交，然后再塞入缓存
            let _ = systeam.commit_transaction(raw_dev);
            //赛入缓存
            systeam.commit_queue.push(updates);
            trace!("[JBD2 BUFFER] BUFFER IS FULL ,FLUSHED!")
        } else {
            //赛入缓存
            systeam.commit_queue.push(updates);
        }

        Ok(())
    }
    pub fn read_block(&mut self, block_id: u32) -> BlockDevResult<()> {
        self.inner.read_block(block_id)
    }
    pub fn buffer(&self) -> &[u8] {
        self.inner.buffer()
    }
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.inner.buffer_mut()
    }
    pub fn read_blocks(&mut self, buf: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()> {
        self.inner.read_blocks(buf, block_id, count)
    }
    pub fn write_blocks(
        &mut self,
        buf: &[u8],
        block_id: u32,
        count: u32,
        is_metadata: bool,
    ) -> BlockDevResult<()> {
        //error!("write block :{} ,use journal?:{} ismetadata:{}",block_id,self.journal_use,is_metadata);

        // 1) 非元数据 或 未开启日志：直接写回到底层块设备
        if !self.journal_use || !is_metadata {
            // BlockDev 内部的 buffer 已经被上层写好，直接把当前 buffer 写到 block_id
            return self.inner.write_blocks(buf, block_id, count);
        }

        // 2) 元数据且启用日志：走 JBD2 事务
        //    此时之前的普通数据块已经完成写入

        //由于分布提交机制，必须需要拷贝数据牺牲性能来确保日志提交
        // 从缓存里拷贝当前要写回的元数据块内容到本地 Vec，避免一直持有对 self.inner 的不可变借用
        let meta_vec = self.inner.buffer();
        let updates = Jbd2Update(
            block_id as u64,
            meta_vec
                .try_into()
                .expect("Data can;t into [u8;BLOCK_SIZE] panic!,os should process"),
        );

        // 注意：在 mkfs/早期阶段可能还没设置 super_block，此时直接退化为普通写，避免阻塞格式化
        if self.systeam.is_none() {
            // 日志标志已开但还没有 journal superblock，暂时按非日志写处理
            error!(
                "Journal systeam uninitial,but journal has turned，this sentence must be once!!!"
            );
            return self.inner.write_block(block_id);
        }

        let systeam = self.systeam.as_mut().unwrap();

        // 使用原始底层块设备提交事务
        let raw_dev = self.inner.device_mut();

        //先写入缓存
        if systeam.commit_queue.len() > JBD2_BUFFER_MAX {
            //缓存已满 直接提交，然后再塞入缓存
            let _ = systeam.commit_transaction(raw_dev);
            //赛入缓存
            systeam.commit_queue.push(updates);
            trace!("[JBD2 BUFFER] BUFFER IS FULL ,FLUSHED!")
        } else {
            //赛入缓存
            systeam.commit_queue.push(updates);
        }


        Ok(())
    }
    pub fn flush(&mut self) -> BlockDevResult<()> {
        if !self.journal_use {
            return self.inner.flush();
        }
        Ok(())
    }

    pub fn total_blocks(&self) -> u64 {
        self.inner.total_blocks()
    }
    pub fn block_size(&self) -> u32 {
        self.inner.block_size()
    }
}

impl<B: BlockDevice> BlockDev<B> {
    /// 创建新的块设备封装
    pub fn new(dev:B) -> Self {
        Self {
            dev,
            buffer: BlockBuffer::new(),
            is_dirty: false,
            cached_block: None,
        }
    }

    /// 使用指定缓冲区初始化块设备
    pub fn _with_buffer(dev:B, buffer: BlockBuffer) -> BlockDevResult<Self> {
        if buffer.len() < 512 {
            return Err(BlockDevError::BufferTooSmall {
                provided: buffer.len(),
                required: 512,
            });
        }

        Ok(Self {
            dev,
            buffer,
            is_dirty: false,
            cached_block: None,
        })
    }

    /// 打开块设备
    pub fn _open(&mut self) -> BlockDevResult<()> {
        self.dev.open()
    }

    /// 关闭块设备
    pub fn _close(&mut self) -> BlockDevResult<()> {
        self.flush()?;
        self.dev.close()
    }

    /// 读取指定块到内部缓冲区
    pub fn read_block(&mut self, block_id: u32) -> BlockDevResult<()> {
        // 检查是否需要刷新脏数据
        if self.is_dirty && self.cached_block != Some(block_id) {
            self.flush()?;
        }

        // 如果已经缓存了该块，直接返回
        if self.cached_block == Some(block_id) {
            return Ok(());
        }

        // 读取块
        self.dev.read(self.buffer.as_mut_slice(), block_id, 1)?;
        self.cached_block = Some(block_id);
        self.is_dirty = false;

        Ok(())
    }

    /// 写入内部缓冲区到指定块
    ///
    pub fn write_block(&mut self, block_id: u32) -> BlockDevResult<()> {
        if self.dev.is_readonly() {
            return Err(BlockDevError::ReadOnly);
        }

        self.dev.write(self.buffer.as_slice(), block_id, 1)?;
        self.cached_block = Some(block_id);
        self.is_dirty = false;

        Ok(())
    }

    /// 直接读取多个块
    pub fn read_blocks(&mut self, buffer: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()> {
        let block_size = self.dev.block_size() as usize;
        let required_size = block_size * count as usize;

        if buffer.len() < required_size {
            return Err(BlockDevError::BufferTooSmall {
                provided: buffer.len(),
                required: required_size,
            });
        }

        self.dev.read(buffer, block_id, count)
    }

    /// 直接写入多个块
    pub fn write_blocks(&mut self, buffer: &[u8], block_id: u32, count: u32) -> BlockDevResult<()> {
        if self.dev.is_readonly() {
            return Err(BlockDevError::ReadOnly);
        }

        let block_size = self.dev.block_size() as usize;
        let required_size = block_size * count as usize;

        if buffer.len() < required_size {
            return Err(BlockDevError::BufferTooSmall {
                provided: buffer.len(),
                required: required_size,
            });
        }

        self.dev.write(buffer, block_id, count)
    }

    /// 获取缓冲区引用
    pub fn buffer(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    /// 获取可变缓冲区引用并标记为脏
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.is_dirty = true;
        self.buffer.as_mut_slice()
    }

    /// 刷新脏缓冲区到磁盘
    pub fn flush(&mut self) -> BlockDevResult<()> {
        if self.is_dirty
            && let Some(block_id) = self.cached_block {
                self.write_block(block_id)?;
            }
        self.dev.flush()
    }

    /// 获取总块数
    pub fn total_blocks(&self) -> u64 {
        self.dev.total_blocks()
    }

    /// 获取块大小
    pub fn block_size(&self) -> u32 {
        self.dev.block_size()
    }

    /// 检查块号是否有效
    pub fn _is_valid_block(&self, block_id: u32) -> bool {
        (block_id as u64) < self.total_blocks()
    }

    /// 验证块范围
    pub fn _validate_block_range(&self, block_id: u32, count: u32) -> BlockDevResult<()> {
        let end_block = block_id as u64 + count as u64;
        if end_block > self.total_blocks() {
            return Err(BlockDevError::BlockOutOfRange {
                block_id,
                max_blocks: self.total_blocks(),
            });
        }
        Ok(())
    }

    /// 获取内部设备引用
    pub fn _device(&self) -> &B {
        &self.dev
    }

    /// 获取内部设备可变引用
    pub fn device_mut(&mut self) -> &mut B {
        &mut self.dev
    }
}
