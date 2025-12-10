#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;
use alloc::{format, vec::{ Vec}};
mod allocator;
mod virtio_blk;
mod console;
mod lang_items;

use core::arch::asm;
use RVlwext4::{Jbd2Dev, api::{open_file, read_from_file}, ext4::*, mkd::mkdir, mkfile::{mkfile, read_file}, BlockDevice};
use alloc::{fmt::format, string::String};
use log::*;
use alloc::vec;
use crate::virtio_blk::VirtIOBlockWrapper;
use RVlwext4::BLOCK_SIZE;

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    clear_bss();
    console::init();
    allocator::init_heap();
    
    
    info!("初始化内存分配器...");
    info!("初始化 VirtIO 块设备...");
    
    match virtio_blk::init_virtio_blk() {
        Ok(()) => {
            info!("VirtIO 块设备初始化成功!");

            // 可选的纯块设备测试
            if let Some(handle) = virtio_blk::get_block_device() {
                // test_block_device(handle);
                let _ = handle; // 避免未使用警告
            }

            // 使用同一个 VirtIOBlockWrapper / Jbd2Dev 串行跑完整的 Ext4 流程
            info!("\n=== 使用单一块设备跑 Ext4 测试流程 ===");
            virtio_blk::with_block_device_mut(|device| {
                // 1) 创建唯一的 Jbd2Dev，开启 journal
                let mut jbd = Jbd2Dev::initial_jbd2dev(0, device, true);

                // 2) mkfs
                info!("\n=== 测试 Ext4 mkfs ===");
                test_ext4_mkfs(&mut jbd);

                // 3) 挂载
                info!("\n=== EXT4 挂载测试 ===");
                let mut fs = test_mount(&mut jbd);

                // 4) 文件查找 / 线性扫描
                test_find_file_line(&mut jbd, &mut fs);

                // 5) 基本 IO 测试
                test_base_io(&mut jbd, &mut fs);

                // 6) 卸载
                test_unmount(&mut jbd, fs);
            });
        }
        Err(e) => {
            error!("VirtIO 块设备初始化失败: {:?}", e);
        }
    }
    
    
    println!("\n=== 测试完成 ===");
    shutdown();
}

///文件夹创建，文件写入修改读测试
fn test_base_io<B: BlockDevice>(block_dev:&mut Jbd2Dev<B>, fs:&mut Ext4FileSystem){
    mkdir(block_dev, fs, "/test_dir/");
    let mut tmp_buffer :[u8;9000]= [b'R';9000];
    let test_str = "Hello ext4 rust!".as_bytes();
    tmp_buffer[8999]=b'L';
    mkfile(block_dev, fs, "/test_dir/testfile", Some(&tmp_buffer));
    mkfile(block_dev, fs, "/testfile2", Some(&test_str));
    let data=read_file(block_dev, fs, "/testfile2").unwrap().unwrap();
    let string = String::from_utf8(data).unwrap();
    let mut file = open_file(block_dev, fs, "/testfile2", false).unwrap();
    let resu=read_from_file(block_dev, fs, &mut file, 10).unwrap();
    error!("offset read:{:?}",String::from_utf8(resu));
    error!("read: {}",string);

    let mut test_big_file:Vec<u8> =vec![b'g';1024*1024*100];//if so large stack will flow,now i want use stack, if you  test larage file please use vec
    for i in 0..1 {
        let file_name =format!("/test_dir/test_file:{}",i);
        mkfile(block_dev, fs, &file_name,Some(&test_big_file));
    }
}
///文件查找测试
fn test_find_file_line<B: BlockDevice>(block_dev:&mut Jbd2Dev<B>,fs:&mut Ext4FileSystem){
   find_file(fs, block_dev, "/.////../.a");
}

///挂载测试
fn test_mount<B: BlockDevice>(block_dev:&mut Jbd2Dev<B>)->Ext4FileSystem{
    debug!("EXT4挂载测试");
    mount(block_dev).expect("Mount Error!")
}
//取消挂载测试
fn test_unmount<B: BlockDevice>(block_dev:&mut Jbd2Dev<B>,fs:Ext4FileSystem){
    debug!("EXT4 umount 测试");
    umount(fs, block_dev);
}


/// 测试块设备
fn test_block_device(handle: virtio_blk::VirtIOBlockDeviceHandle) {
    use RVlwext4::BlockDevice;
    use alloc::vec;
    
    info!("开始块设备测试...");
    
    // 测试1: 基本读写
    info!("测试1: 基本读写");
    
    // 写入测试
    handle.with_device_mut(|device| {
        let write_data = [0x42u8; BLOCK_SIZE];
        match device.write(&write_data, 0, 1) {
            Ok(()) => info!("  写入成功"),
            Err(e) => error!("  写入失败: {:?}", e),
        };
        let mut write_data2 = [0u8; BLOCK_SIZE];
        write_data2[0]=22;
        write_data2[3]=44;
        match device.write(&write_data2, 20, 1){
             Ok(()) => info!("  test_data2写入成功"),
            Err(e) => error!("  test_data2写入失败: {:?}", e),
        }
        
    });
    
    // 读取测试
    handle.with_device(|device| {
        let write_data = [0x42u8; BLOCK_SIZE];
        let mut read_data = [0u8; BLOCK_SIZE];
        match device.read(&mut read_data, 0, 1) {
            Ok(()) => {
                let matches = read_data == write_data;
                if matches {
                    info!("  读取成功，数据匹配 ✓");
                } else {
                    error!("  数据不匹配 ✗");
                }
            }
            Err(e) => error!("  读取失败: {:?}", e),
        }
    });
    handle.with_device(|device|{
        let mut read_data = [0u8; BLOCK_SIZE];
        match device.read(&mut read_data,20, 1){
            Ok(()) => {
                let verify_data = read_data[0] + read_data[3];
                if  verify_data!= 66{
                    error!("  Data Read Success But verify failed!: Expect {} But read {}",66,verify_data)
                }else {
                    debug!("Verify data :{} {} Success!",read_data[0],read_data[3]);                    
                }
            }
            Err(e) => error!("  读取失败: {:?}", e),
        }
    });
    
    // 测试2: 多块读写
    info!("测试2: 多块读写");
    
    // 准备写入数据
    let mut multi_write = vec![0u8; BLOCK_SIZE * 3];
    for i in 0..3 {
        for j in 0..BLOCK_SIZE {
            multi_write[i * BLOCK_SIZE + j] = ((i + j) % 256) as u8;
        }
    }
    
    // 写入多块
    let multi_write_clone = multi_write.clone();
    handle.with_device_mut(|device| {
        match device.write(&multi_write_clone, 10, 3) {
            Ok(()) => info!("  写入3个块成功"),
            Err(e) => error!("  写入失败: {:?}", e),
        }
    });
    
    // 读取多块
    handle.with_device(|device| {
        let mut multi_read = vec![0u8; BLOCK_SIZE * 3];
        match device.read(&mut multi_read, 10, 3) {
            Ok(()) => {
                let matches = multi_read == multi_write;
                if matches {
                    info!("  读取成功，数据匹配 ✓");
                } else {
                    error!("  数据不匹配 ✗");
                }
            }
            Err(e) => error!("  读取失败: {:?}", e),
        }
    });
    
    info!("块设备测试完成!");
}

/// 测试 Ext4 格式化（在已创建的 Jbd2Dev 上执行）
fn test_ext4_mkfs<B: BlockDevice>(block_dev: &mut Jbd2Dev<B>) {
    info!("开始格式化 Ext4 文件系统...");

    info!("  设备容量: {} 块", block_dev.total_blocks());

    match mkfs(block_dev) {
        Ok(()) => {
            info!("✓ Ext4 文件系统格式化成功!");
        }
        Err(e) => {
            error!("✗ 格式化失败: {:?}", e);
        }
    }
}

/// 清除 BSS 段
fn clear_bss() {
    extern "C" {
        fn sbss();
        fn ebss();
    }
    unsafe {
        core::slice::from_raw_parts_mut(
            sbss as usize as *mut u8,
            ebss as usize - sbss as usize,
        )
        .fill(0);
    }
}

/// 关机
fn shutdown() -> ! {
    // QEMU virt 机器的 poweroff
    const VIRT_TEST: *mut u32 = 0x100000 as *mut u32;
    unsafe {
        VIRT_TEST.write_volatile(0x5555); // QEMU test device poweroff
    }
    loop {
        unsafe { asm!("wfi") }
    }
}

/// 全局 println 宏
#[macro_export]
macro_rules! println {
    () => ($crate::console::console_putchar(b'\n'));
    ($($arg:tt)*) => ({
        $crate::console::_print(format_args!($($arg)*));
        $crate::console::console_putchar(b'\n');
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        $crate::console::_print(format_args!($($arg)*));
    });
}
