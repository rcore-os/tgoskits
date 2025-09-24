use axerrno::AxResult;
use axtask::current;
use starry_core::task::AsThread;

pub fn sys_brk(addr: usize) -> AxResult<isize> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let mut return_val: isize = proc_data.get_heap_top() as isize;
    let heap_bottom = proc_data.get_heap_bottom() as usize;
    if addr != 0 && addr >= heap_bottom && addr <= heap_bottom + starry_core::config::USER_HEAP_SIZE
    {
        proc_data.set_heap_top(addr);
        return_val = addr as isize;
    }
    Ok(return_val)
}
