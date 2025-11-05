use core::{any::Any, ffi::c_int};

use axfs_ng_vfs::{DeviceId, NodeFlags, VfsError, VfsResult};
use chrono::{Datelike, Timelike};
use linux_raw_sys::ioctl::RTC_RD_TIME;
use starry_vm::VmMutPtr;

use crate::vfs::DeviceOps;

/// The device ID for /dev/rtc0
pub const RTC0_DEVICE_ID: DeviceId = DeviceId::new(250, 0);

#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
struct rtc_time {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
}

/// RTC device
pub struct Rtc;

impl DeviceOps for Rtc {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            RTC_RD_TIME => {
                let wall =
                    chrono::DateTime::from_timestamp_nanos(axhal::time::wall_time_nanos() as _);
                (arg as *mut rtc_time).vm_write(rtc_time {
                    tm_sec: wall.second() as _,
                    tm_min: wall.minute() as _,
                    tm_hour: wall.hour() as _,
                    tm_mday: wall.day() as _,
                    tm_mon: wall.month0() as _,
                    tm_year: (wall.year() - 1900) as _,
                    tm_wday: 0,
                    tm_yday: 0,
                    tm_isdst: 0,
                })?;
            }
            _ => return Err(VfsError::NotATty),
        }
        Ok(0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}
