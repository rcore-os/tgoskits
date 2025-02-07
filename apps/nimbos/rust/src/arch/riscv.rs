use core::arch::{asm, naked_asm};

use crate::syscall::{SYSCALL_CLONE, SYSCALL_EXIT};

pub fn syscall(id: usize, args: [usize; 3]) -> isize {
    let ret;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") args[0] => ret,
            in("a1") args[1],
            in("a2") args[2],
            in("a7") id,
        );
    }
    ret
}

#[naked]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn sys_clone(_entry: fn(usize) -> i32, _arg: usize, _newsp: usize) -> isize {
    // sys_clone(entry, arg, newsp)
    //             a0,   a1,    a2
    // syscall(SYSCALL_CLONE, newsp)
    //                   a7,     x0
    unsafe {
        naked_asm!("
            // align stack and save entry,arg to the new stack
            andi    a2, a2, -16
            addi    a2, a2, -16
            sd      a0, 0(a2)
            sd      a1, 8(a2)

            // syscall(SYSCALL_CLONE, newsp)
            mv      a0, a2
            li      a7, {sys_clone}
            ecall

            beqz    a0, 2f
            // parent
            ret
        2:
            // child
            ld      a0, 8(sp)
            ld      a1, 0(sp)
            jalr    a1
            // syscall(SYSCALL_EXIT, ret)
            li      a7, {sys_exit}
            ecall",
            sys_clone = const SYSCALL_CLONE,
            sys_exit = const SYSCALL_EXIT
        )
    }
}
