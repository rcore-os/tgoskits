use core::arch::{asm, naked_asm};

use crate::syscall::{SYSCALL_CLONE, SYSCALL_EXIT};

pub fn syscall(id: usize, args: [usize; 3]) -> isize {
    let ret;
    unsafe {
        asm!(
            "svc #0",
            inlateout("x0") args[0] => ret,
            in("x1") args[1],
            in("x2") args[2],
            in("x8") id,
        );
    }
    ret
}

#[naked]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn sys_clone(_entry: fn(usize) -> i32, _arg: usize, _newsp: usize) -> isize {
    // sys_clone(entry, arg, newsp)
    //             x0,   x1,    x2
    // syscall(SYSCALL_CLONE, newsp)
    //                   x8,     x0
    unsafe {
        naked_asm!("
            // align stack and save entry,arg to the new stack
            and x2, x2, #-16
            stp x0, x1, [x2, #-16]!

            // syscall(SYSCALL_CLONE, newsp)
            mov x0, x2
            mov x8, {sys_clone}
            svc #0

            cbz x0, 2f
            // parent
            ret
        2:
            // child
            ldp x1, x0, [sp], #16
            blr x1
            // syscall(SYSCALL_EXIT, ret)
            mov x8, {sys_exit}
            svc #0",
            sys_clone = const SYSCALL_CLONE,
            sys_exit = const SYSCALL_EXIT
        )
    }
}
