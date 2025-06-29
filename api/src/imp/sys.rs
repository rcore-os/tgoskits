use core::ffi::c_char;

use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FS_CONTEXT;
use linux_raw_sys::{
    general::{
        __user_cap_data_struct, __user_cap_header_struct, GRND_INSECURE, GRND_NONBLOCK, GRND_RANDOM,
    },
    system::{new_utsname, sysinfo},
};
use starry_core::task::{get_process, processes};

use crate::ptr::UserPtr;

pub fn sys_getuid() -> LinuxResult<isize> {
    Ok(0)
}

pub fn sys_geteuid() -> LinuxResult<isize> {
    Ok(0)
}

pub fn sys_getgid() -> LinuxResult<isize> {
    Ok(0)
}

pub fn sys_getegid() -> LinuxResult<isize> {
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
    sysname: pad_str("Starry"),
    nodename: pad_str("Starry - machine[0]"),
    release: pad_str("10.0.0"),
    version: pad_str("10.0.0"),
    machine: pad_str("10.0.0"),
    domainname: pad_str("https://github.com/oscomp/starry-next"),
};

pub fn sys_uname(name: UserPtr<new_utsname>) -> LinuxResult<isize> {
    *name.get_as_mut()? = UTSNAME;
    Ok(0)
}

pub fn sys_sysinfo(info: UserPtr<sysinfo>) -> LinuxResult<isize> {
    let info = info.get_as_mut()?;
    info.uptime = 0;
    info.loads = [0, 0, 0];
    info.totalram = 0;
    info.freeram = 0;
    info.sharedram = 0;
    info.bufferram = 0;
    info.totalswap = 0;
    info.freeswap = 0;
    info.procs = processes().len() as _;
    info.totalhigh = 0;
    info.freehigh = 0;
    info.mem_unit = 1;
    Ok(0)
}

pub fn sys_syslog(_type: i32, _buf: UserPtr<c_char>, _len: usize) -> LinuxResult<isize> {
    Ok(0)
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct GetRandomFlags: u32 {
        const NONBLOCK = GRND_NONBLOCK;
        const RANDOM = GRND_RANDOM;
        const INSECURE = GRND_INSECURE;
    }
}

pub fn sys_getrandom(buf: UserPtr<u8>, len: usize, flags: u32) -> LinuxResult<isize> {
    if len == 0 {
        return Ok(0);
    }
    let buf = buf.get_as_mut_slice(len)?;
    let flags = GetRandomFlags::from_bits_retain(flags);

    debug!(
        "sys_getrandom <= buf: {:p}, len: {}, flags: {:?}",
        buf, len, flags
    );

    let path = if flags.contains(GetRandomFlags::RANDOM) {
        "/dev/random"
    } else {
        "/dev/urandom"
    };

    let mut f = axfs_ng::File::open(&FS_CONTEXT.lock(), path)?;
    let len = f.read_at(buf, 0)?;

    Ok(len as _)
}

fn validate_cap_header(header: &mut __user_cap_header_struct) -> LinuxResult<()> {
    if header.version != 0x20080522 {
        header.version = 0x20080522;
        return Err(LinuxError::EINVAL);
    }
    let _ = get_process(header.pid as u32)?;
    Ok(())
}

pub fn sys_capget(
    header: UserPtr<__user_cap_header_struct>,
    data: UserPtr<__user_cap_data_struct>,
) -> LinuxResult<isize> {
    let header = header.get_as_mut()?;
    validate_cap_header(header)?;

    *data.get_as_mut()? = __user_cap_data_struct {
        effective: u32::MAX,
        permitted: u32::MAX,
        inheritable: u32::MAX,
    };
    Ok(0)
}

pub fn sys_capset(
    header: UserPtr<__user_cap_header_struct>,
    _data: UserPtr<__user_cap_data_struct>,
) -> LinuxResult<isize> {
    let header = header.get_as_mut()?;
    validate_cap_header(header)?;

    Ok(0)
}
