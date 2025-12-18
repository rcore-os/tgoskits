 # rsext4
 
 **如何使用**。
 
 ## 1. 接入块设备（实现 `BlockDevice` trait）
 
 你需要提供一个块设备实现，并实现 `rsext4::BlockDevice`：
 
 - **`read(&mut self, buffer, block_id, count)`**：从 `block_id` 开始读取 `count` 个块到 `buffer`
 - **`write(&mut self, buffer, block_id, count)`**：从 `buffer` 写入 `count` 个块到设备
 - **`open/close`**：可选的设备打开/关闭（可为空实现）
 - **`total_blocks()`**：设备总块数
 - **`block_size()`**：块大小（通常为 `BLOCK_SIZE`）
 
 下面是一个参考实现：使用宿主机文件模拟块设备（来自 `src/main.rs`）。
 
 ```rust
 use std::fs::{File, OpenOptions};
 use std::io::{Read, Seek, SeekFrom, Write};
 use std::path::Path;
 
 use rsext4::{BlockDevError, BlockDevResult, BlockDevice, BLOCK_SIZE};
 
 struct FileBlockDev {
     file: File,
     total_blocks: u64,
 }
 
 impl FileBlockDev {
     fn open_or_create<P: AsRef<Path>>(path: P, total_blocks: u64) -> std::io::Result<Self> {
         let path = path.as_ref();
         let block_size = BLOCK_SIZE as u64;
         let size_bytes = total_blocks * block_size;
 
         let file = OpenOptions::new()
             .read(true)
             .write(true)
             .create(true)
             .open(path)?;
 
         let metadata = file.metadata()?;
         if metadata.len() < size_bytes {
             file.set_len(size_bytes)?;
         }
 
         Ok(Self { file, total_blocks })
     }
 }
 
 impl BlockDevice for FileBlockDev {
     fn write(&mut self, buffer: &[u8], block_id: u32, count: u32) -> BlockDevResult<()> {
         let block_size = self.block_size() as usize;
         let required = block_size * count as usize;
         if buffer.len() < required {
             return Err(BlockDevError::BufferTooSmall { provided: buffer.len(), required });
         }
 
         let offset = block_id as u64 * block_size as u64;
         let bytes = &buffer[..required];
 
         self.file.seek(SeekFrom::Start(offset)).map_err(|_| BlockDevError::IoError)?;
         self.file.write_all(bytes).map_err(|_| BlockDevError::IoError)?;
         self.file.flush().map_err(|_| BlockDevError::IoError)?;
         Ok(())
     }
 
     fn read(&mut self, buffer: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()> {
         let block_size = self.block_size() as usize;
         let required = block_size * count as usize;
         if buffer.len() < required {
             return Err(BlockDevError::BufferTooSmall { provided: buffer.len(), required });
         }
 
         let offset = block_id as u64 * block_size as u64;
         let mut f = &self.file;
         f.seek(SeekFrom::Start(offset)).map_err(|_| BlockDevError::IoError)?;
         f.read_exact(&mut buffer[..required]).map_err(|_| BlockDevError::IoError)?;
         Ok(())
     }
 
     fn open(&mut self) -> BlockDevResult<()> { Ok(()) }
 
     fn close(&mut self) -> BlockDevResult<()> {
         self.file.flush().map_err(|_| BlockDevError::IoError)?;
         Ok(())
     }
 
     fn total_blocks(&self) -> u64 { self.total_blocks }
 
     fn block_size(&self) -> u32 { BLOCK_SIZE as u32 }
 }
 ```
 
 ## 2. 用 `Jbd2Dev` 包装块设备,目前只支持ordered模式,ordered会储存完整元数据内容然后写主盘，如果对性能有较高要求请关闭
  
 `rsext4` 的所有读写都通过 `Jbd2Dev<B>` 进行：
 
 ```rust
 use rsext4::Jbd2Dev;
 
 // _mode: 日志级别（当前实现里只用 0 - ordered）
 // use_journal: 是否启用 journaling
 let mut dev = Jbd2Dev::initial_jbd2dev(0, block_dev, /*use_journal=*/ true);
 ```
 
 运行时可以切换 journal 开关：
 如果在mount之后启用需要手动读取日志超级块并且注入!
 
 ```rust
 dev.set_journal_use(true);
 dev.set_journal_use(false);
 ```
 
 如果你需要手动重放日志（注意：会有性能影响）：
 
 ```rust
 dev.journal_replay();
 ```
 
 注意：`mkfs()` 内部会临时关闭 journal，避免在 journal superblock 尚未注入时触发 JBD2 逻辑；`mkfs()` 结束前会恢复原先的开关状态（见 `src/ext4_backend/ext4.rs`）。
 
 ## 3. 创建文件系统（mkfs）
 
 ```rust
 use rsext4::mkfs;
 
 mkfs(&mut dev)?;
 ```
 
 ## 4. 挂载与卸载
 
 你可以直接用 `mount/umount`，也可以用 `fs_mount/fs_umount`（它们只是转发到 `ext4::mount/umount`）。
 
 ```rust
 use rsext4::{mount, umount};
 
 let mut fs = mount(&mut dev)?;
 
 // ... 对 fs 进行各种操作 ...
 
 umount(fs, &mut dev)?; //数据块缓存，inode缓存，同步超级块，同步块组描述符
 ```
 
 ## 5. 常用 API 使用
 
 下面这些调用方式来自 `src/testfs/test_example.rs`（建议直接看该文件作为更完整的用例集合）。
 
 ### 5.1 目录与文件创建
 
 ```rust
 use rsext4::{mkdir, mkfile};
 
 mkdir(&mut dev, &mut fs, "/test_dir/");
 
 let data = vec![b'a'; 4096];
 mkfile(&mut dev, &mut fs, "/test_dir/hello", Some(&data),None);//最后的是文件类型，仅仅作用于inode标志和entry标志。对数据结构不产生任何影响
 mkfile(&mut dev, &mut fs, "/test_dir/empty", None,None);
 ```
 
 ### 5.2 读取整个文件
 
 ```rust
 use rsext4::read_file;
 
 let content = read_file(&mut dev, &mut fs, "/test_dir/hello")?;
 if let Some(bytes) = content {
     // bytes: Vec<u8>
 }
 ```
 
 ### 5.3 打开文件句柄 + 基于 offset 的写入/读取
 
 `open()` 返回 `OpenFile { path, inode, offset }`，并维护 `offset`。
 
 ```rust
 use rsext4::{open, append, read_at, lseek};
 
 let mut f = open(&mut dev, &mut fs, "/test_dir/f", true)?;
 
 append(&mut dev, &mut fs, &mut f, b"hello")?;
 append(&mut dev, &mut fs, &mut f, b" world")?;
 
 // 移动 offset
 let ok = lseek(&mut f, 0);
 assert!(ok);
 
 // 从当前 offset 读取最多 len 字节；会更新 f.offset
 let buf = read_at(&mut dev, &mut fs, &mut f, 5)?;
 ```
 
 ### 5.4 rename / mv
 
 ```rust
 use rsext4::{rename, mv};
 
 rename(&mut dev, &mut fs, "/renametest/a", "/renametest/c")?;
 mv(&mut fs, &mut dev, "/mvtest/a/f1", "/mvtest/b/f1_moved")?;
 ```
 
 ### 5.5 link / unlink
 
 ```rust
 use rsext4::{link, unlink};
 
 link(&mut fs, &mut dev, "/linktest/l1", "/linktest/target");
 unlink(&mut fs, &mut dev, "/linktest/l1");
 ```
 
 ### 5.6 符号链接
 
 ```rust
 use rsext4::create_symbol_link;
 
 create_symbol_link(&mut dev, &mut fs, "/symlinktest/target", "/symlinktest/l1")?;
 ```
 
 ### 5.7 truncate
 
 ```rust
 use rsext4::truncate;
 
 truncate(&mut dev, &mut fs, "/truncatetest/f1", 0)?;
 truncate(&mut dev, &mut fs, "/truncatetest/f1", 128 * 1024)?;
 ```
 
 ### 5.8 删除
 
 ```rust
 use rsext4::{delete_file, delete_dir};
 
 delete_file(&mut fs, &mut dev, "/path/to/file");
 delete_dir(&mut fs, &mut dev, "/path/to/dir");
 ```
 

 ## 6.注意，目前数据完整性依赖umount时的flush来把所有缓存落盘，如果不使用umount请手动flush
 ```rust
        // Flush dirty caches
        self.bitmap_cache.flush_all(block_dev)?;
        self.inodetable_cahce.flush_all(block_dev)?;
        self.datablock_cache.flush_all(block_dev)?;

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

        // 4. Update superblock
        self.sync_superblock(block_dev)?;
        self.sync_group_descriptors(block_dev)?;

        //确保缓存已经提交完毕
        block_dev.umount_commit();

 ```
 


