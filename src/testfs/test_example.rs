use crate::ext4_backend::loopfile::get_file_inode;
use rsext4::*;
use std::io::Read;
use std::io::Write;
//mkfs
pub fn test_mkfs<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) {
    mkfs(block_dev).expect("File system mount failed panic!");
}
/// 文件写入/读取测试
pub fn _test_base_io<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/test_dir/");
    // 大文件测试：写入 + 读取 吞吐量
    let test_big_file: Vec<u8> = vec![b'g'; 1024 * 1024 * 2001]; // 2001MB
    let file_count = 1u64;
    let total_write_bytes = test_big_file.len() as u64;
    let write_start = std::time::Instant::now();
    for i in 0..file_count {
        let file_name = format!("/test_dir/test_file:{i}");
        mkfile(block_dev, fs, &file_name, Some(&test_big_file));
    }
    //数据实际落盘
    fs.datablock_cache.flush_all(block_dev).expect("Bitmap Flsuh failed!");
    fs.inodetable_cahce.flush_all(block_dev).expect("Inodetable Flsuh failed!");
    fs.bitmap_cache.flush_all(block_dev).expect("Bitmap Flsuh failed!");
    let write_duration = write_start.elapsed();
    let write_secs = write_duration.as_secs_f64();
    let write_mib = total_write_bytes as f64 / (1024.0 * 1024.0);
    let write_mib_s = if write_secs > 0.0 {
        write_mib / write_secs
    } else {
        0.0
    };
    println!(
        "大文件写入: total={write_mib:.2} MiB, time={write_secs:.3} s, speed={write_mib_s:.2} MiB/s"
    );

    // 读取吞吐量测试：依次读回刚才写入的几个大文件
    let read_start = std::time::Instant::now();
    let mut read_bytes: u64 = 0;
    for i in 0..file_count {
        let file_name = format!("/test_dir/test_file:{i}");
        if let Some(data) = read_file(block_dev, fs, &file_name).unwrap() {
            read_bytes += data.len() as u64;
        }
    }
    let read_duration = read_start.elapsed();
    let read_secs = read_duration.as_secs_f64();
    let read_mib = read_bytes as f64 / (1024.0 * 1024.0);
    let read_mib_s = if read_secs > 0.0 {
        read_mib / read_secs
    } else {
        0.0
    };
    println!(
        "大文件读取: total={read_mib:.2} MiB, time={read_secs:.3} s, speed={read_mib_s:.2} MiB/s"
    );

    //=== 宿主机文件系统: 相同规模的大文件写入/读取测试 ===
    let host_path = "host_fs_test.bin";
    let total_bytes = test_big_file.len() as u64;

    // 宿主机写入
    let host_write_start = std::time::Instant::now();
    {
        let mut f = std::fs::File::create(host_path).expect("create host fs test file failed");
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
        "[HOST FS] 写入: total={host_write_mib:.2} MiB, time={host_write_secs:.3} s, speed={host_write_mib_s:.2} MiB/s"
    );

    // 宿主机读取
    let host_read_start = std::time::Instant::now();
    let mut host_read_buf = vec![0u8; test_big_file.len()];
    {
        let mut f = std::fs::File::open(host_path).expect("open host fs test file failed");
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
        "[HOST FS] 读取: total={host_read_mib:.2} MiB, time={host_read_secs:.3} s, speed={host_read_mib_s:.2} MiB/s"
    );
}

/// 文件删除测试
pub fn test_delete<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    let test_big_file: Vec<u8> = vec![b'g'; 1024 * 1024 * 20]; // 20MB
    for idx in 0..10 {
        let file_name = format!("/deltest/childdir/file:{idx}");
        mkfile(block_dev, fs, &file_name, Some(&test_big_file));
    }
    delete_dir(fs, block_dev, "/deltest");
}

pub fn test_link<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/linktest_link");

    let payload: Vec<u8> = (0..(1024 * 1024)).map(|i| (i % 251) as u8).collect();
    mkfile(block_dev, fs, "/linktest_link/target", Some(&payload));

    link(fs, block_dev, "/linktest_link/l1", "/linktest_link/target");

    let (ino_target, _) = get_file_inode(fs, block_dev, "/linktest_link/target")
        .ok()
        .flatten()
        .expect("target inode missing after mkfile");
    let (ino_link, _) = get_file_inode(fs, block_dev, "/linktest_link/l1")
        .ok()
        .flatten()
        .expect("link inode missing after link");
    assert_eq!(ino_target, ino_link);

    let data_target = read_file(block_dev, fs, "/linktest_link/target")
        .unwrap()
        .expect("read target failed");
    let data_link = read_file(block_dev, fs, "/linktest_link/l1")
        .unwrap()
        .expect("read link failed");
    assert_eq!(data_target, payload);
    assert_eq!(data_link, payload);
}

pub fn test_unlink<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/linktest_unlink");

    let payload: Vec<u8> = (0..(1024 * 1024)).map(|i| (i % 251) as u8).collect();
    mkfile(block_dev, fs, "/linktest_unlink/target", Some(&payload));
    link(
        fs,
        block_dev,
        "/linktest_unlink/l1",
        "/linktest_unlink/target",
    );

    unlink(fs, block_dev, "/linktest_unlink/l1");
    assert!(
        get_file_inode(fs, block_dev, "/linktest_unlink/l1")
            .ok()
            .flatten()
            .is_none()
    );
    assert!(
        get_file_inode(fs, block_dev, "/linktest_unlink/target")
            .ok()
            .flatten()
            .is_some()
    );

    let data_target2 = read_file(block_dev, fs, "/linktest_unlink/target")
        .unwrap()
        .expect("read target after unlink failed");
    assert_eq!(data_target2, payload);

    delete_file(fs, block_dev, "/linktest_unlink/target");
    assert!(
        get_file_inode(fs, block_dev, "/linktest_unlink/target")
            .ok()
            .flatten()
            .is_none()
    );
}

pub fn test_symbol_link<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/symlinktest");

    let payload: Vec<u8> = (0..(64 * 1024)).map(|i| (i % 251) as u8).collect();
    mkfile(block_dev, fs, "/symlinktest/target", Some(&payload));

    create_symbol_link(block_dev, fs, "/symlinktest/target", "/symlinktest/l1")
        .expect("create_symbol_link failed");

    let (_ino_link, inode_link) = get_file_inode(fs, block_dev, "/symlinktest/l1")
        .ok()
        .flatten()
        .expect("symlink inode missing after create_symbol_link");
    assert!(inode_link.is_symlink());

    let data_via_link = read_file(block_dev, fs, "/symlinktest/l1")
        .unwrap()
        .expect("read symlink-follow failed");
    assert_eq!(data_via_link, payload);
}

pub fn test_truncate<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/truncatetest");

    let payload: Vec<u8> = (0..(64 * 1024)).map(|i| (i % 251) as u8).collect();
    mkfile(block_dev, fs, "/truncatetest/f1", Some(&payload));

    // shrink to non-zero (cross block boundary)
    let shrink_len: u64 = (BLOCK_SIZE + 123) as u64;
    truncate(block_dev, fs, "/truncatetest/f1", shrink_len).expect("truncate shrink failed");
    let data_shrink = read_file(block_dev, fs, "/truncatetest/f1")
        .unwrap()
        .expect("read after truncate shrink failed");
    assert_eq!(data_shrink.len() as u64, shrink_len);
    assert_eq!(&data_shrink[..], &payload[..shrink_len as usize]);

    // truncate to same size should be no-op
    truncate(block_dev, fs, "/truncatetest/f1", shrink_len).expect("truncate same size failed");
    let data_same = read_file(block_dev, fs, "/truncatetest/f1")
        .unwrap()
        .expect("read after truncate same size failed");
    assert_eq!(data_same, data_shrink);

    // truncate -> 0
    truncate(block_dev, fs, "/truncatetest/f1", 0).expect("truncate to 0 failed");
    let data0 = read_file(block_dev, fs, "/truncatetest/f1")
        .unwrap()
        .expect("read after truncate(0) failed");
    assert!(data0.is_empty());

    // grow：新空间应为 0
    let new_len: u64 = (BLOCK_SIZE + 17) as u64;
    truncate(block_dev, fs, "/truncatetest/f1", new_len).expect("truncate grow failed");
    let data1 = read_file(block_dev, fs, "/truncatetest/f1")
        .unwrap()
        .expect("read after truncate grow failed");
    assert_eq!(data1.len() as u64, new_len);
    assert!(data1.iter().all(|&b| b == 0));

    // shrink on sparse file: create a hole then truncate to 0 (should not double free)
    mkfile(block_dev, fs, "/truncatetest/f_sparse", None);
    write_file(block_dev, fs, "/truncatetest/f_sparse", 0, b"ABC").unwrap();
    write_file(
        block_dev,
        fs,
        "/truncatetest/f_sparse",
        BLOCK_SIZE * 3,
        b"XYZ",
    )
    .unwrap();
    truncate(block_dev, fs, "/truncatetest/f_sparse", 0).expect("truncate sparse->0 failed");
    let data_sparse0 = read_file(block_dev, fs, "/truncatetest/f_sparse")
        .unwrap()
        .expect("read sparse after truncate(0) failed");
    assert!(data_sparse0.is_empty());
}

pub fn test_api_write_at_read_at<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    fs: &mut Ext4FileSystem,
) {
    mkdir(block_dev, fs, "/apiiotest");

    let mut f = open(block_dev, fs, "/apiiotest/f1", true).expect("open failed");

    // write_at appends at current offset
    write_at(block_dev, fs, &mut f, b"HELLO").expect("write_at failed");
    assert_eq!(f.offset, 5);

    // create a hole by seeking forward, then write again
    assert!(lseek(&mut f, BLOCK_SIZE + 10));
    write_at(block_dev, fs, &mut f, b"WORLD").expect("write_at 2 failed");

    // Ensure inode metadata is up-to-date for subsequent assertions.
    let Some((_ino, inode_now)) = get_file_inode(fs, block_dev, "/apiiotest/f1")
        .ok()
        .flatten()
    else {
        panic!("inode missing after writes");
    };
    assert!(inode_now.size() as usize >= BLOCK_SIZE + 10 + 5);

    // read back from start across the hole: hole bytes should be zeros
    assert!(lseek(&mut f, 0));
    let want = BLOCK_SIZE + 10 + 5;
    let got = read_at(block_dev, fs, &mut f, want).expect("read_at failed");
    assert_eq!(got.len(), want);
    assert_eq!(&got[..5], b"HELLO");
    assert!(got[5..BLOCK_SIZE + 10].iter().all(|&b| b == 0));
    assert_eq!(&got[BLOCK_SIZE + 10..BLOCK_SIZE + 10 + 5], b"WORLD");

    // offset advanced by logical bytes read
    assert_eq!(f.offset, BLOCK_SIZE + 10 + 5);
}

pub fn _test_journal_powerfail<B: BlockDevice>(
    block_dev: &mut Jbd2Dev<B>,
    mut fs: Ext4FileSystem,
) -> Ext4FileSystem {
    // This test only makes sense when journal is enabled.
    block_dev.set_journal_use(true);

    mkdir(block_dev, &mut fs, "/journaltest");
    mkfile(block_dev, &mut fs, "/journaltest/f1", None);

    let payload = b"JOURNAL_PAYLOAD_123456";
    write_file(block_dev, &mut fs, "/journaltest/f1", 0, payload)
        .expect("write_file failed");

    // Flush caches to generate journaled metadata updates (inode table, bitmaps, etc.).
    fs.datablock_cache
        .flush_all(block_dev)
        .expect("flush datablock failed");
    fs.inodetable_cahce
        .flush_all(block_dev)
        .expect("flush inode table failed");
    fs.bitmap_cache
        .flush_all(block_dev)
        .expect("flush bitmap failed");

    // Commit the journal transaction, but do NOT call fs.umount (simulate power loss).
    block_dev.umount_commit();
    drop(fs);

    // Remount: ext4::mount will inject journal superblock and replay.
    let mut fs2 = mount(block_dev).expect("remount failed");

    // After replay, inode size/metadata should be visible, and file should read correctly.
    let got = read_file(block_dev, &mut fs2, "/journaltest/f1")
        .unwrap()
        .expect("read after replay failed");
    assert_eq!(got, payload);

    // Restore default behavior for subsequent tests.
    fs2
}

pub fn test_rename<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/renametest");

    let payload_a: Vec<u8> = (0..(32 * 1024)).map(|i| (i % 251) as u8).collect();
    let payload_b: Vec<u8> = (0..(16 * 1024)).map(|i| ((i + 7) % 251) as u8).collect();

    mkfile(block_dev, fs, "/renametest/a", Some(&payload_a));
    mkfile(block_dev, fs, "/renametest/b", Some(&payload_b));

    // rename a -> c
    rename(block_dev, fs, "/renametest/a", "/renametest/c").expect("rename a->c failed");
    assert!(
        get_file_inode(fs, block_dev, "/renametest/a")
            .ok()
            .flatten()
            .is_none()
    );
    let c = read_file(block_dev, fs, "/renametest/c")
        .unwrap()
        .expect("read /renametest/c failed");
    assert_eq!(c, payload_a);

    // overwrite: rename b -> c (c exists)
    rename(block_dev, fs, "/renametest/b", "/renametest/c").expect("rename b->c overwrite failed");
    assert!(
        get_file_inode(fs, block_dev, "/renametest/b")
            .ok()
            .flatten()
            .is_none()
    );
    let c2 = read_file(block_dev, fs, "/renametest/c")
        .unwrap()
        .expect("read /renametest/c after overwrite failed");
    assert_eq!(c2, payload_b);
}



pub fn test_mv<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/mvtest");
    mkdir(block_dev, fs, "/mvtest/a");
    mkdir(block_dev, fs, "/mvtest/b");

    let payload: Vec<u8> = (0..(128 * 1024)).map(|i| (i % 251) as u8).collect();
    mkfile(block_dev, fs, "/mvtest/a/f1", Some(&payload));

    mv(fs, block_dev, "/mvtest/a/f1", "/mvtest/a/f1_renamed").expect("mv rename failed");
    assert!(
        get_file_inode(fs, block_dev, "/mvtest/a/f1")
            .ok()
            .flatten()
            .is_none()
    );
    let data1 = read_file(block_dev, fs, "/mvtest/a/f1_renamed")
        .unwrap()
        .expect("read moved file failed");
    assert_eq!(data1, payload);

    mv(fs, block_dev, "/mvtest/a/f1_renamed", "/mvtest/b/f1_moved").expect("mv cross-dir failed");
    assert!(
        get_file_inode(fs, block_dev, "/mvtest/a/f1_renamed")
            .ok()
            .flatten()
            .is_none()
    );
    let data2 = read_file(block_dev, fs, "/mvtest/b/f1_moved")
        .unwrap()
        .expect("read moved-across file failed");
    assert_eq!(data2, payload);

    // directory move across parents
    mkdir(block_dev, fs, "/mvtest/dir1");
    mkfile(block_dev, fs, "/mvtest/dir1/inner", Some(&payload));
    mkdir(block_dev, fs, "/mvtest/dir2");

    mv(fs, block_dev, "/mvtest/dir1", "/mvtest/dir2/dir1_moved").expect("mv dir failed");
    assert!(
        get_file_inode(fs, block_dev, "/mvtest/dir1")
            .ok()
            .flatten()
            .is_none()
    );
    let data3 = read_file(block_dev, fs, "/mvtest/dir2/dir1_moved/inner")
        .unwrap()
        .expect("read inner file after dir mv failed");
    assert_eq!(data3, payload);
}

/// 文件写入测试
pub fn test_normal_apiuse<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    //make many file and dir
    mkdir(block_dev, fs, "/test/hello");
    let test_big_file: Vec<u8> = vec![b'g'; 1024 * 1024 * 20]; // 20MB
    for idx in 0..10 {
        let file_name = format!("/test/hello/test{idx}");
        mkfile(block_dev, fs, &file_name, Some(&test_big_file));
    }
}

/// 文件查找测试\
pub fn test_find_file_line<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    find_file(fs, block_dev, "/.////../.a");
}

/// 挂载测试
pub fn test_mount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) -> Ext4FileSystem {
    mount(block_dev).expect("Mount Error!")
}

/// 取消挂载测试
pub fn test_unmount<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: Ext4FileSystem) {
    umount(fs, block_dev).expect("File system umount failed panic!");
}
