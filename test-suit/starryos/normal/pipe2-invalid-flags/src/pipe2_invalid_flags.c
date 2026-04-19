#define SYS_close 57
#define SYS_pipe2 59
#define SYS_write 64
#define SYS_exit_group 94

#define EINVAL 22
#define BAD_PIPE2_FLAG 0x40UL

typedef unsigned long ulong;
typedef unsigned long size_t;

static long syscall1(long nr, long arg0) {
    register long a0 asm("a0") = arg0;
    register long a7 asm("a7") = nr;
    asm volatile("ecall" : "+r"(a0) : "r"(a7) : "memory");
    return a0;
}

static long syscall2(long nr, long arg0, long arg1) {
    register long a0 asm("a0") = arg0;
    register long a1 asm("a1") = arg1;
    register long a7 asm("a7") = nr;
    asm volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a7) : "memory");
    return a0;
}

static long syscall3(long nr, long arg0, long arg1, long arg2) {
    register long a0 asm("a0") = arg0;
    register long a1 asm("a1") = arg1;
    register long a2 asm("a2") = arg2;
    register long a7 asm("a7") = nr;
    asm volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a2), "r"(a7) : "memory");
    return a0;
}

static size_t cstr_len(const char *s) {
    size_t len = 0;
    while (s[len] != '\0') {
        len++;
    }
    return len;
}

static void write_cstr(int fd, const char *s) {
    syscall3(SYS_write, fd, (long)s, (long)cstr_len(s));
}

static void write_long(int fd, long value) {
    char buf[32];
    char *cur = &buf[31];
    ulong magnitude;

    *cur = '\0';
    if (value < 0) {
        magnitude = (ulong)(-value);
    } else {
        magnitude = (ulong)value;
    }

    do {
        *--cur = (char)('0' + (magnitude % 10));
        magnitude /= 10;
    } while (magnitude != 0);

    if (value < 0) {
        *--cur = '-';
    }

    syscall3(SYS_write, fd, (long)cur, (long)cstr_len(cur));
}

static void fail_with_ret(const char *prefix, long ret) {
    write_cstr(2, "FAIL: ");
    write_cstr(2, prefix);
    write_long(2, ret);
    write_cstr(2, "\n");
    syscall1(SYS_exit_group, 1);
}

static void check_valid_pipe2(void) {
    int fds[2] = {-1, -1};
    long ret = syscall2(SYS_pipe2, (long)fds, 0);
    if (ret != 0) {
        fail_with_ret("pipe2(valid) returned ", ret);
    }

    syscall1(SYS_close, fds[0]);
    syscall1(SYS_close, fds[1]);
}

static void check_invalid_pipe2_flags(void) {
    int fds[2] = {-1, -1};
    long ret = syscall2(SYS_pipe2, (long)fds, (long)BAD_PIPE2_FLAG);
    if (ret == 0) {
        syscall1(SYS_close, fds[0]);
        syscall1(SYS_close, fds[1]);
        fail_with_ret("pipe2(invalid-flags) unexpectedly returned ", ret);
    }
    if (ret != -EINVAL) {
        fail_with_ret("pipe2(invalid-flags) returned ", ret);
    }
}

void _start(void) {
    check_valid_pipe2();
    check_invalid_pipe2_flags();
    write_cstr(1, "pipe2-invalid-flags: ok\n");
    syscall1(SYS_exit_group, 0);
}
