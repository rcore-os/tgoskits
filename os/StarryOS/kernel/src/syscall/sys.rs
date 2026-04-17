use alloc::vec;
use core::ffi::c_char;

use ax_config::ARCH;
use ax_errno::{AxError, AxResult};
use ax_fs::FS_CONTEXT;
use linux_raw_sys::{
    general::{GRND_INSECURE, GRND_NONBLOCK, GRND_RANDOM},
    system::{new_utsname, sysinfo},
};
use starry_vm::{VmMutPtr, vm_write_slice};

use crate::task::processes;

pub fn sys_getuid() -> AxResult<isize> {
    Ok(0)
}

pub fn sys_geteuid() -> AxResult<isize> {
    Ok(0)
}

pub fn sys_getgid() -> AxResult<isize> {
    Ok(0)
}

pub fn sys_getegid() -> AxResult<isize> {
    Ok(0)
}

pub fn sys_setuid(_uid: u32) -> AxResult<isize> {
    debug!("sys_setuid <= uid: {_uid}");
    Ok(0)
}

pub fn sys_setgid(_gid: u32) -> AxResult<isize> {
    debug!("sys_setgid <= gid: {_gid}");
    Ok(0)
}

pub fn sys_getgroups(size: usize, list: *mut u32) -> AxResult<isize> {
    debug!("sys_getgroups <= size: {size}");
    if size < 1 {
        return Err(AxError::InvalidInput);
    }
    vm_write_slice(list, &[0])?;
    Ok(1)
}

pub fn sys_setgroups(_size: usize, _list: *const u32) -> AxResult<isize> {
    Ok(0)
}

const fn pad_str(info: &str) -> [c_char; 65] {
    let mut data: [c_char; 65] = [0; 65];
    // this needs #![feature(const_copy_from_slice)]
    // data[..info.len()].copy_from_slice(info.as_bytes());
    unsafe {
        core::ptr::copy_nonoverlapping(info.as_ptr().cast(), data.as_mut_ptr(), info.len());
    }
    data
}

const UTSNAME: new_utsname = new_utsname {
    sysname: pad_str("Linux"),
    nodename: pad_str("starry"),
    release: pad_str("10.0.0"),
    version: pad_str("10.0.0"),
    machine: pad_str(ARCH),
    domainname: pad_str("https://github.com/Starry-OS/StarryOS"),
};

pub fn sys_uname(name: *mut new_utsname) -> AxResult<isize> {
    name.vm_write(UTSNAME)?;
    Ok(0)
}

pub fn sys_sysinfo(info: *mut sysinfo) -> AxResult<isize> {
    let mut kinfo = sysinfo {
        uptime: 0,
        loads: [0; 3],
        totalram: 0,
        freeram: 0,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: processes().len() as _,
        pad: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        ..Default::default()
    };
    info.vm_write(kinfo)?;
    Ok(0)
}

pub fn sys_syslog(type_: i32, _buf: *mut c_char, _len: usize) -> AxResult<isize> {
    debug!("sys_syslog called! type: {}, len: {}", type_, _len);
    
    // TODO: 这是一个模拟实现，后续需要对接内核真实的日志读取和控制逻辑
    match type_ {
        2 | 3 | 4 => Ok(0),
        5 | 6 | 7 | 8 => Ok(0),
        9 => Ok(0),
        10 => Ok(4096), // 模拟返回系统日志缓冲区大小
        _ => Ok(0),
    }
}
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct GetRandomFlags: u32 {
        const NONBLOCK = GRND_NONBLOCK;
        const RANDOM = GRND_RANDOM;
        const INSECURE = GRND_INSECURE;
    }
}

pub fn sys_getrandom(buf: *mut u8, len: usize, flags: u32) -> AxResult<isize> {
    if len == 0 {
        return Ok(0);
    }
    let flags = GetRandomFlags::from_bits_retain(flags);

    debug!("sys_getrandom <= buf: {buf:p}, len: {len}, flags: {flags:?}");

    let path = if flags.contains(GetRandomFlags::RANDOM) {
        "/dev/random"
    } else {
        "/dev/urandom"
    };

    let f = FS_CONTEXT.lock().resolve(path)?;
    let mut kbuf = vec![0; len];
    let len = f.entry().as_file()?.read_at(&mut kbuf, 0)?;

    vm_write_slice(buf, &kbuf)?;

    Ok(len as _)
}

pub fn sys_seccomp(_op: u32, _flags: u32, _args: *const ()) -> AxResult<isize> {
    warn!("dummy sys_seccomp");
    Ok(0)
}

#[cfg(target_arch = "riscv64")]
pub fn sys_riscv_flush_icache() -> AxResult<isize> {
    riscv::asm::fence_i();
    Ok(0)
}
// 获取当前线程所在的 CPU 核心编号
pub fn sys_getcpu(cpu: *mut u32, node: *mut u32) -> AxResult<isize> {
    info!("sys_getcpu called! cpu ptr: {:?}, node ptr: {:?}", cpu, node);
    
    // TODO: 这是一个模拟实现，目前仅返回 0，后续需要对接真实的 CPU 核心和 NUMA 节点获取逻辑
    // 在内核底层直接操作指针是非常危险的，必须用 unsafe 块包裹
    unsafe {
        // 如果外部程序传进来的 cpu 指针不是空的，我们就把 0 写进去
        if !cpu.is_null() {
            *cpu = 0; 
        }
        // 同理，把 NUMA 节点也默认写为 0
        if !node.is_null() {
            *node = 0;
        }
    }
    
    Ok(0)
}
    
    Ok(0)
}
