use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use log::*;
use RVlwext4::*;
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
            Level::Warn  => ("WARN ", "\x1b[33m"), // 黄
            Level::Info  => ("INFO ", "\x1b[32m"), // 绿
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

        let mut file = OpenOptions::new()
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

    fn read(&self, buffer: &mut [u8], block_id: u32, count: u32) -> BlockDevResult<()> {
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
        Ok("info")  => LevelFilter::Info,
        Ok("warn")  => LevelFilter::Warn,
        Ok("error") => LevelFilter::Error,
        _ => LevelFilter::Off, // 默认
    };
    log::set_max_level(level);

    // 简单地创建一个 512MB 的镜像文件
    let blocks: u64 = (512u64 * 1024 * 1024) / (BLOCK_SIZE as u64);
    let img_path = "ext4.img";

    info!("使用宿主机文件作为块设备: {} (blocks={}, block_size={})", img_path, blocks, BLOCK_SIZE);

    let mut host_dev = match FileBlockDev::open_or_create(img_path, blocks) {
        Ok(dev) => dev,
        Err(e) => {
            eprintln!("打开/创建镜像文件失败: {}", e);
            return;
        }
    };

    // 包一层 Jbd2Dev，开启 journal
    let mut jbd = Jbd2Dev::initial_jbd2dev(0, &mut host_dev, true);

    info!("=== 测试 Ext4 mkfs ===");
    test_mkfs(&mut jbd);

    info!("=== EXT4 挂载测试 ===");
    let mut fs = test_mount(&mut jbd);

    info!("=== 文件查找测试 ===");
    test_find_file_line(&mut jbd, &mut fs);

    info!("=== 基本 IO 测试 ===");
    test_base_io(&mut jbd, &mut fs);

    info!("=== 卸载测试 ===");
    test_unmount(&mut jbd, fs);

    info!("=== 测试完成 ===");
}


//mkfs
fn test_mkfs<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) {
    mkfs(block_dev);
}


/// 文件夹创建，文件写入/读取测试
fn test_base_io<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/test_dir/");

    let mut tmp_buffer: [u8; 9000] = [b'R'; 9000];
    let test_str = b"Hello ext4 rust!";
    tmp_buffer[8999] = b'L';
    mkfile(block_dev, fs, "/test_dir/testfile", Some(&tmp_buffer));
    mkfile(block_dev, fs, "/testfile2", Some(test_str));

    let data = read_file(block_dev, fs, "/testfile2").unwrap().unwrap();
    let string = String::from_utf8(data).unwrap();

    let mut file = open_file(block_dev, fs, "/testfile2", false).unwrap();
    let resu = read_from_file(block_dev, fs, &mut file, 10).unwrap();
    error!("offset read: {:?}", String::from_utf8(resu));
    error!("read: {}", string);

    // 大文件测试：写入 + 读取 吞吐量
    let test_big_file: Vec<u8> = vec![b'g'; 1024 * 1024 * 200]; // 200MB
    let file_count = 1u64; // 写入 3 个大文件，避免内存占用过大
    let total_write_bytes = test_big_file.len() as u64;

    let write_start = std::time::Instant::now();
    for i in 0..file_count {
        let file_name = format!("/test_dir/test_file:{}", i);
        mkfile(block_dev, fs, &file_name, Some(&test_big_file));
    }
    //数据实际落盘
    fs.datablock_cache.flush_all(block_dev);
    fs.inodetable_cahce.flush_all(block_dev);
    fs.bitmap_cache.flush_all(block_dev);
    let write_duration = write_start.elapsed();
    let write_secs = write_duration.as_secs_f64();
    let write_mib = total_write_bytes as f64 / (1024.0 * 1024.0);
    let write_mib_s = if write_secs > 0.0 { write_mib / write_secs } else { 0.0 };
    println!(
        "大文件写入: total={:.2} MiB, time={:.3} s, speed={:.2} MiB/s",
        write_mib, write_secs, write_mib_s
    );

    // 读取吞吐量测试：依次读回刚才写入的几个大文件
    let read_start = std::time::Instant::now();
    let mut read_bytes: u64 = 0;
    for i in 0..file_count {
        let file_name = format!("/test_dir/test_file:{}", i);
        if let Some(data) = read_file(block_dev, fs, &file_name).unwrap() {
            read_bytes += data.len() as u64;
        }
    }
    let read_duration = read_start.elapsed();
    let read_secs = read_duration.as_secs_f64();
    let read_mib = read_bytes as f64 / (1024.0 * 1024.0);
    let read_mib_s = if read_secs > 0.0 { read_mib / read_secs } else { 0.0 };
    println!(
        "大文件读取: total={:.2} MiB, time={:.3} s, speed={:.2} MiB/s",
        read_mib, read_secs, read_mib_s
    );

    //=== 宿主机文件系统: 相同规模的大文件写入/读取测试 ===
    let host_path = "host_fs_test.bin";
    let total_bytes = test_big_file.len() as u64;

    // 宿主机写入
    let host_write_start = std::time::Instant::now();
    {
        let mut f = std::fs::File::create(host_path)
            .expect("create host fs test file failed");
        f.write_all(&test_big_file)
            .expect("write host fs test file failed");
        f.flush().expect("flush host fs test file failed");
    }
    let host_write_dur = host_write_start.elapsed();
    let host_write_secs = host_write_dur.as_secs_f64();
    let host_write_mib = total_bytes as f64 / (1024.0 * 1024.0);
    let host_write_mib_s = if host_write_secs > 0.0 {
        host_write_mib / host_write_secs
    } else {
        0.0
    };
    println!(
        "[HOST FS] 写入: total={:.2} MiB, time={:.3} s, speed={:.2} MiB/s",
        host_write_mib, host_write_secs, host_write_mib_s
    );

    // 宿主机读取
    let host_read_start = std::time::Instant::now();
    let mut host_read_buf = vec![0u8; test_big_file.len()];
    {
        let mut f = std::fs::File::open(host_path)
            .expect("open host fs test file failed");
        f.read_exact(&mut host_read_buf)
            .expect("read host fs test file failed");
    }
    let host_read_dur = host_read_start.elapsed();
    let host_read_secs = host_read_dur.as_secs_f64();
    let host_read_mib = total_bytes as f64 / (1024.0 * 1024.0);
    let host_read_mib_s = if host_read_secs > 0.0 {
        host_read_mib / host_read_secs
    } else {
        0.0
    };
    println!(
        "[HOST FS] 读取: total={:.2} MiB, time={:.3} s, speed={:.2} MiB/s",
        host_read_mib, host_read_secs, host_read_mib_s
    );
}

/// 文件查找测试
fn test_find_file_line<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    find_file(fs, block_dev, "/.////../.a");
}

/// 挂载测试
fn test_mount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> Ext4FileSystem {
    debug!("EXT4挂载测试");
    mount(block_dev).expect("Mount Error!")
}

/// 取消挂载测试
fn test_unmount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: Ext4FileSystem) {
    debug!("EXT4 umount 测试");
    umount(fs, block_dev);
}
