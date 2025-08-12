use axerrno::LinuxResult;

pub fn sys_membarrier(_cmd: i32, _flags: u32, _cpu_id: i32) -> LinuxResult<isize> {
    info!("Dummy membarrier called");
    Ok(0)
}
