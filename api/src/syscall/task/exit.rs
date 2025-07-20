use axerrno::LinuxResult;

use crate::task::do_exit;

pub fn sys_exit(exit_code: i32) -> LinuxResult<isize> {
    do_exit(exit_code << 8, false);
    Ok(0)
}

pub fn sys_exit_group(exit_code: i32) -> LinuxResult<isize> {
    do_exit(exit_code << 8, true);
    Ok(0)
}
