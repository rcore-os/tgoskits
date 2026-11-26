/*
 * PID 1 wrapper for the StarryOS benchmark initramfs.
 *
 * The benchmark runs as a child so its normal exit is not treated as PID 1
 * exiting. A failed benchmark or wait reports a distinct failure marker.
 */

typedef unsigned long u64;

#define AT_FDCWD (-100)
#define O_RDWR 02

#define SYS_DUP3 24
#define SYS_OPENAT 56
#define SYS_WRITE 64
#define SYS_PPOLL 73
#define SYS_EXIT 94
#define SYS_CLONE 220
#define SYS_EXECVE 221
#define SYS_WAIT4 260

#define SIGCHLD 17

static int console_fd = -1;
static unsigned char child_stack[64 * 1024] __attribute__((aligned(16)));

__asm__(
    ".global _start\n"
    ".type _start, %function\n"
    "_start:\n"
    "mov x0, sp\n"
    "bl init_main\n"
    "mov x0, #1\n"
    "mov x8, #94\n"
    "svc #0\n"
    "1:\n"
    "b 1b\n"
    ".size _start, .-_start\n"
);

static long syscall1(long number, long a0) {
    register long x0 __asm__("x0") = a0;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory", "cc");
    return x0;
}

static long syscall3(long number, long a0, long a1, long a2) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x8)
                     : "memory", "cc");
    return x0;
}

static long syscall4(long number, long a0, long a1, long a2, long a3) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x3), "r"(x8)
                     : "memory", "cc");
    return x0;
}

static long syscall5(long number, long a0, long a1, long a2, long a3, long a4) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8)
                     : "memory", "cc");
    return x0;
}

static u64 text_len(const char *text) {
    u64 length = 0;
    while (text[length] != '\0') {
        length++;
    }
    return length;
}

static void write_all(const char *text) {
    const char *cursor = text;
    u64 remaining = text_len(text);
    while (remaining != 0) {
        long written = syscall3(SYS_WRITE, console_fd, (long)cursor, (long)remaining);
        if (written <= 0) {
            return;
        }
        cursor += written;
        remaining -= (u64)written;
    }
}

static void open_console(void) {
    long fd = syscall3(SYS_OPENAT, AT_FDCWD, (long)"/dev/ttyS0", O_RDWR);
    if (fd < 0) {
        fd = 1;
    }
    console_fd = (int)fd;
    if (fd >= 0) {
        syscall3(SYS_DUP3, fd, 1, 0);
        syscall3(SYS_DUP3, fd, 2, 0);
    }
}

__attribute__((noreturn)) static void stable_idle(void) {
    for (;;) {
        syscall5(SYS_PPOLL, 0, 0, 0, 0, 8);
    }
}

__attribute__((noreturn)) static void init_fail(const char *reason) {
    write_all("AXIVC_STARRY_INIT FAIL reason=");
    write_all(reason);
    write_all("\n");
    stable_idle();
}

void init_main(unsigned long *stack) {
    (void)stack;
    open_console();

    static char bench_path[] = "/usr/bin/ivc-starry-bench";
    static char *const argv[] = {bench_path, 0};
    static char home_env[] = "HOME=/root";
    static char path_env[] = "PATH=/usr/bin:/bin";
    static char term_env[] = "TERM=dumb";
    static char *const envp[] = {home_env, path_env, term_env, 0};

    long child = syscall5(
        SYS_CLONE,
        SIGCHLD,
        (long)(child_stack + sizeof(child_stack)),
        0,
        0,
        0
    );
    if (child < 0) {
        init_fail("clone");
    }
    if (child == 0) {
        syscall3(SYS_EXECVE, (long)bench_path, (long)argv, (long)envp);
        syscall1(SYS_EXIT, 127);
    }

    int status = 0;
    long waited = syscall4(SYS_WAIT4, child, (long)&status, 0, 0);
    if (waited != child) {
        init_fail("wait4");
    }
    if ((status & 0x7f) != 0 || ((status >> 8) & 0xff) != 0) {
        init_fail("benchmark-exit");
    }

    write_all("AXIVC_STARRY_INIT BENCHMARK_EXIT=0 stable=idle\n");
    stable_idle();
}
