#![deny(unused)]
#![deny(dead_code)]
#![deny(warnings)]
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
mod testfs;
use crate::testfs::*;
use rsext4::*;
use log::*;
struct SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // 这里可以按需要过滤级别
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        // 根据级别选颜色
        let (level_str, color) = match record.level() {
            Level::Error => ("ERROR", "\x1b[31m"), // 红
            Level::Warn => ("WARN ", "\x1b[33m"),  // 黄
            Level::Info => ("INFO ", "\x1b[32m"),  // 绿
            Level::Debug => ("DEBUG", "\x1b[34m"), // 蓝
            Level::Trace => ("TRACE", "\x1b[90m"), // 灰
        };

        let reset = "\x1b[0m";

        println!(
            "{}[{}]{} {}: {}",
            color,
            level_str,
            reset,
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {}
}

// 全局静态实例
static LOGGER: SimpleLogger = SimpleLogger;

/// 简单的基于宿主机文件的块设备实现
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

        // 预分配文件大小
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
            return Err(BlockDevError::BufferTooSmall {
                provided: buffer.len(),
                required,
            });
        }

        let offset = block_id as u64 * block_size as u64;
        let bytes = &buffer[..required];

        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|_| BlockDevError::IoError)?;
        self.file
            .write_all(bytes)
            .map_err(|_| BlockDevError::IoError)?;
        self.file.flush().map_err(|_| BlockDevError::IoError)?;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()> {
        let block_size = self.block_size() as usize;
        let required = block_size * count as usize;
        if buffer.len() < required {
            return Err(BlockDevError::BufferTooSmall {
                provided: buffer.len(),
                required,
            });
        }

        let offset = block_id as u64 * block_size as u64;

        let mut f = &self.file;
        f.seek(SeekFrom::Start(offset))
            .map_err(|_| BlockDevError::IoError)?;
        f.read_exact(&mut buffer[..required])
            .map_err(|_| BlockDevError::IoError)?;
        Ok(())
    }

    fn open(&mut self) -> BlockDevResult<()> {
        Ok(())
    }

    fn close(&mut self) -> BlockDevResult<()> {
        self.file.flush().map_err(|_| BlockDevError::IoError)?;
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    fn block_size(&self) -> u32 {
        BLOCK_SIZE as u32
    }
}

fn main() {
    // 注册自定义 logger
    log::set_logger(&LOGGER).unwrap();
    // 从环境读取日志等级，比如 RUST_LOG=debug / info / error
    let level = match std::env::var("LOG").as_deref() {
        Ok("trace") => LevelFilter::Trace,
        Ok("debug") => LevelFilter::Debug,
        Ok("info") => LevelFilter::Info,
        Ok("warn") => LevelFilter::Warn,
        Ok("error") => LevelFilter::Error,
        _ => LevelFilter::Off, // 默认
    };
    log::set_max_level(level);

    // 简单地创建一个 8G 的镜像文件
    let blocks: u64 = (8192u64 * 1024 * 1024) / (BLOCK_SIZE as u64);
    let img_path = "ext4.img";

    info!(
        "使用宿主机文件作为块设备: {img_path} (blocks={blocks}, block_size={BLOCK_SIZE})"
    );

    let host_dev = match FileBlockDev::open_or_create(img_path, blocks) {
        Ok(dev) => dev,
        Err(e) => {
            eprintln!("打开/创建镜像文件失败: {e}");
            return;
        }
    };

    // 包一层 Jbd2Dev，开启 journal
    let mut jbd = Jbd2Dev::initial_jbd2dev(0, host_dev, false);

    info!("=== 测试 Ext4 mkfs ===");
    test_mkfs(&mut jbd);


    // Enable journaling for mounted filesystem operations.
    //jbd.set_journal_use(true);

    info!("=== EXT4 挂载测试 ===");
    let mut fs = test_mount(&mut jbd);

    info!("=== 文件查找测试 ===");
    test_find_file_line(&mut jbd, &mut fs);

    info!("=== 基本 IO 测试 ===");
    _test_base_io(&mut jbd, &mut fs);

    test_normal_apiuse(&mut jbd, &mut fs);

    info!("=== 删除 测试 ===");
    test_delete(&mut jbd, &mut fs);

    info!("=== link 测试 ===");
    test_link(&mut jbd, &mut fs);

    info!("=== unlink 测试 ===");
    test_unlink(&mut jbd, &mut fs);

    info!("=== mv 测试 ===");
    test_mv(&mut jbd, &mut fs);

    info!("=== create symbol link 测试 ===");
    test_symbol_link(&mut jbd, &mut fs);

    info!("=== truncate 测试 ===");
    test_truncate(&mut jbd, &mut fs);

    info!("=== api_write_at_read_at 测试 ===");
    test_api_write_at_read_at(&mut jbd, &mut fs);

    info!("=== journal 断电回放 测试 ===");
    //fs = test_journal_poweerfail(&mut jbd, fs);
    
    info!("=== rename 测试 ===");
    test_rename(&mut jbd, &mut fs);

    info!("=== 卸载测试 ===");
    test_unmount(&mut jbd, fs);

    info!("=== 测试完成 ===");
}
