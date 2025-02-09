use core::arch::asm;

pub fn syscall(id: usize, args: [usize; 3]) -> isize {
    let ret;
    unsafe {
        asm!(
            "syscall 0",
            inlateout("$r4") args[0] => ret,
            in("$r5") args[1],
            in("$r6") args[2],
            in("$r11") id,
        );
    }
    ret
}

#[allow(improper_ctypes_definitions)]
pub extern "C" fn sys_clone(_entry: fn(usize) -> i32, _arg: usize, _newsp: usize) -> isize {
    // sys_clone(entry, arg, newsp)
    //             a0,   a1,    a2
    // syscall(SYSCALL_CLONE, newsp)
    //                   a7,     x0
    unimplemented!()
}
