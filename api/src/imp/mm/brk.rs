use axerrno::LinuxResult;
use axtask::{TaskExtRef, current};
use macro_rules_attribute::apply;

use crate::syscall_instrument;

#[apply(syscall_instrument)]
pub fn sys_brk(addr: usize) -> LinuxResult<isize> {
    let task = current();
    let process_data = task.task_ext().process_data();
    let mut return_val: isize = process_data.get_heap_top() as isize;
    let heap_bottom = process_data.get_heap_bottom() as usize;
    if addr != 0 && addr >= heap_bottom && addr <= heap_bottom + axconfig::plat::USER_HEAP_SIZE {
        process_data.set_heap_top(addr);
        return_val = addr as isize;
    }
    Ok(return_val)
}
