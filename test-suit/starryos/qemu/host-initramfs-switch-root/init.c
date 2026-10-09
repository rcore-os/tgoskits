static long syscall5(long number, long a0, long a1, long a2, long a3, long a4)
{
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x5 __asm__("x5") = 0;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8) : "memory");
    return x0;
}

static int check(long result, const char *stage, long length)
{
    if (result >= 0) return 1;
    syscall5(64, 1, (long)stage, length, 0, 0);
    char code[24];
    int cursor = 23;
    code[cursor] = '\n';
    unsigned long error = (unsigned long)-result;
    do { code[--cursor] = '0' + error % 10; error /= 10; } while (error);
    syscall5(64, 1, (long)&code[cursor], 24 - cursor, 0, 0);
    return 0;
}

long init_main(unsigned long *stack)
{
    static const char failure[] = "STARRY_USER_ROOT_SWITCH_FAILED\n";
    char **argv = (char **)&stack[1];
    char **envp = argv + stack[0] + 1;
    long old_fd = syscall5(56, -100, (long)"/etc/issue", 0, 0, 0);
    char original[64];
    long size = old_fd >= 0 ? syscall5(67, old_fd, (long)original, sizeof(original), 0, 0) : -1;
    long mapping = size > 0 ? syscall5(222, 0, 4096, 1, 2, old_fd) : -1;
    int valid = size > 0 && mapping >= 0;
    valid = valid && check(syscall5(34, -100, (long)"/newroot", 0755, 0, 0), "mkdir errno=", 12);
    valid = valid && check(syscall5(40, (long)"/dev/nvme0n1", (long)"/newroot", (long)"ext4", 0, 0), "mount errno=", 12);
    valid = valid && check(syscall5(40, (long)"/dev", (long)"/newroot/dev", 0, 8192, 0), "move-dev errno=", 15);
    valid = valid && check(syscall5(49, (long)"/newroot", 0, 0, 0, 0), "chdir-new errno=", 16);
    valid = valid && check(syscall5(41, (long)".", (long)".", 0, 0, 0), "pivot errno=", 12);
    valid = valid && check(syscall5(39, (long)".", 2, 0, 0, 0), "umount-old errno=", 17);
    valid = valid && check(syscall5(49, (long)"/", 0, 0, 0, 0), "chdir-root errno=", 17);
    char preserved[64];
    valid = valid && syscall5(63, old_fd, (long)preserved, sizeof(preserved), 0, 0) == size;
    for (long i = 0; valid && i < size; ++i) valid = original[i] == preserved[i];
    if (old_fd >= 0) valid = check(syscall5(57, old_fd, 0, 0, 0, 0), "close-old errno=", 16) && valid;
    for (long i = 0; valid && i < size; ++i) valid = original[i] == ((volatile char *)mapping)[i];
    if (mapping >= 0) valid = check(syscall5(215, mapping, 4096, 0, 0, 0), "munmap errno=", 13) && valid;
    long fd = valid ? syscall5(56, -100, (long)"/etc/alpine-release", 0, 0, 0) : -1;
    char release[32];
    valid = valid && fd >= 0 && syscall5(63, fd, (long)release, sizeof(release), 0, 0) > 0;
    if (fd >= 0) syscall5(57, fd, 0, 0, 0, 0);
    if (valid) {
        char *shell_args[] = {"/bin/sh", "-c", "echo STARRY_USER_ROOT_SWITCH_PASSED; exec /sbin/init", 0};
        check(syscall5(221, (long)shell_args[0], (long)shell_args, (long)envp, 0, 0), "exec errno=", 11);
    }
    syscall5(64, 1, (long)failure, sizeof(failure) - 1, 0, 0);
    syscall5(142, 0xfee1dead, 672274793, 0x4321fedc, 0, 0);
    for (;;) __asm__ volatile("wfe");
}
