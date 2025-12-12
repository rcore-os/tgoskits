use crate::ext4_backend::loopfile::get_file_inode;
use rsext4::*;
use std::io::Read;
use std::io::Write;
//mkfs
pub fn test_mkfs<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) {
    mkfs(block_dev).expect("File system mount failed panic!");
}
/// 文件写入/读取测试
pub fn test_base_io<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
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



pub fn test_mv<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>, fs: &mut Ext4FileSystem) {
    mkdir(block_dev, fs, "/mvtest");
    mkdir(block_dev, fs, "/mvtest/a");
    mkdir(block_dev, fs, "/mvtest/b");

    let payload: Vec<u8> = (0..(128 * 1024)).map(|i| (i % 251) as u8).collect();
    mkfile(block_dev, fs, "/mvtest/a/f1", Some(&payload));

    mv(fs, block_dev, "/mvtest/a/f1", "/mvtest/a/f1_renamed");
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

    mv(fs, block_dev, "/mvtest/a/f1_renamed", "/mvtest/b/f1_moved");
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

    mv(fs, block_dev, "/mvtest/dir1", "/mvtest/dir2/dir1_moved");
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
