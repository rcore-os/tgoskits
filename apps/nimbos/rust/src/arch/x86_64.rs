use core::arch::asm;

use crate::syscall::{SYSCALL_CLONE, SYSCALL_EXIT};

pub fn syscall(id: usize, args: [usize; 3]) -> isize {
    let ret;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") id => ret,
            in("rdi") args[0],
            in("rsi") args[1],
            in("rdx") args[2],
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

#[naked]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn sys_clone(_entry: fn(usize) -> i32, _arg: usize, _newsp: usize) -> isize {
    // sys_clone(entry, arg, newsp)
    //             rdi, rsi,   rdx
    // syscall(SYSCALL_CLONE, newsp)
    //                   rax,   rdi
    unsafe {
        asm!("
            // push arg (rsi) to stack, set func (rdi) to r9
            and rdx, -16
            sub rdx, 8
            mov [rdx], rsi
            mov r9, rdi

            // syscall(SYSCALL_CLONE, newsp)
            mov rdi, rdx
            mov rax, {sys_clone}
            syscall

            test rax, rax
            jz  1f
            // parent
            ret
        1:
            // child
            xor rbp, rbp
            pop rdi
            call r9
            // syscall(SYSCALL_EXIT, ret)
            mov rdi, rax
            mov rax, {sys_exit}
            syscall",
            sys_clone = const SYSCALL_CLONE,
            sys_exit = const SYSCALL_EXIT,
            options(noreturn),
        )
    }
}
