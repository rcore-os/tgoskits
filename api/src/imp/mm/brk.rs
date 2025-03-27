use axerrno::LinuxResult;
use axtask::{TaskExtRef, current};
use macro_rules_attribute::apply;

use crate::syscall_instrument;

#[apply(syscall_instrument)]
pub fn sys_brk(addr: usize) -> LinuxResult<isize> {
    let current_task = current();
    let mut return_val: isize = current_task.task_ext().get_heap_top() as isize;
    let heap_bottom = current_task.task_ext().get_heap_bottom() as usize;
    if addr != 0 && addr >= heap_bottom && addr <= heap_bottom + axconfig::plat::USER_HEAP_SIZE {
        current_task.task_ext().set_heap_top(addr as u64);
        return_val = addr as isize;
    }
    Ok(return_val)
}
