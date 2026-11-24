static long syscall3(long number, long a0, long a1, long a2)
{
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x8) : "memory");
    return x0;
}

static long syscall4(long number, long a0, long a1, long a2, long a3)
{
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x2), "r"(x3), "r"(x8) : "memory");
    return x0;
}

static int same_text(const char *left, const char *right)
{
    while (*left && *left == *right) {
        left++;
        right++;
    }
    return *left == *right;
}

long init_main(unsigned long *stack)
{
    static const char issue_path[] = "/etc/issue";
    static const char issue_text[] = "TGOS host initramfs\n";
    static const char success[] = "STARRY_INITRAMFS_PASSED\n";
    static const char failure[] = "STARRY_INITRAMFS_FAILED\n";
    char issue[sizeof(issue_text)] = {0};
    long argc = (long)stack[0];
    char **argv = (char **)&stack[1];
    char **envp = argv + argc + 1;
    int valid = argc == 2 && same_text(argv[1], "from-dashes");
    int env_found = 0;
    int unsupported_kernel_env_found = 0;
    int kernel_env_absent = 1;
    for (char **env = envp; *env; env++) {
        if (same_text(*env, "HOST_ENV=ready")) env_found = 1;
        if (same_text(*env, "memmap=exactmap")) unsupported_kernel_env_found = 1;
        if (same_text(*env, "nr_cpus=1") || same_text(*env, "earlyprintk=serial") ||
            same_text(*env, "oops=panic")) kernel_env_absent = 0;
    }
    long fd = syscall4(56, -100, (long)issue_path, 0, 0);
    long read_size = fd < 0 ? -1 : syscall3(63, fd, (long)issue, sizeof(issue));
    valid = valid && env_found && unsupported_kernel_env_found && kernel_env_absent &&
            read_size == sizeof(issue_text) - 1;
    for (unsigned long i = 0; valid && i < sizeof(issue_text) - 1; i++) {
        valid = issue[i] == issue_text[i];
    }
    const char *message = valid ? success : failure;
    unsigned long size = valid ? sizeof(success) - 1 : sizeof(failure) - 1;
    syscall3(64, 1, (long)message, size);
    syscall3(81, 0, 0, 0);
    syscall4(142, 0xfee1dead, 672274793, 0x4321fedc, 0);
    for (;;) __asm__ volatile("wfe");
}
