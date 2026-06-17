#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

struct sock_filter {
    unsigned short code;
    unsigned char jt;
    unsigned char jf;
    unsigned int k;
};

struct sock_fprog {
    unsigned short len;
    struct sock_filter *filter;
};

#ifndef BPF_LD
#define BPF_LD 0x00
#endif
#ifndef BPF_W
#define BPF_W 0x00
#endif
#ifndef BPF_ABS
#define BPF_ABS 0x20
#endif
#ifndef BPF_JMP
#define BPF_JMP 0x05
#endif
#ifndef BPF_JEQ
#define BPF_JEQ 0x10
#endif
#ifndef BPF_K
#define BPF_K 0x00
#endif
#ifndef BPF_RET
#define BPF_RET 0x06
#endif
#ifndef BPF_STMT
#define BPF_STMT(code, k) { (unsigned short)(code), 0, 0, k }
#endif
#ifndef BPF_JUMP
#define BPF_JUMP(code, k, jt, jf) { (unsigned short)(code), jt, jf, k }
#endif
#ifndef SECCOMP_SET_MODE_STRICT
#define SECCOMP_SET_MODE_STRICT 0
#endif
#ifndef SECCOMP_SET_MODE_FILTER
#define SECCOMP_SET_MODE_FILTER 1
#endif
#ifndef SECCOMP_GET_ACTION_AVAIL
#define SECCOMP_GET_ACTION_AVAIL 2
#endif
#ifndef SECCOMP_FILTER_FLAG_TSYNC
#define SECCOMP_FILTER_FLAG_TSYNC (1U << 0)
#endif
#ifndef SECCOMP_RET_KILL_THREAD
#define SECCOMP_RET_KILL_THREAD 0x00000000U
#endif
#ifndef SECCOMP_RET_KILL_PROCESS
#define SECCOMP_RET_KILL_PROCESS 0x80000000U
#endif
#ifndef SECCOMP_RET_TRAP
#define SECCOMP_RET_TRAP 0x00030000U
#endif
#ifndef SECCOMP_RET_ERRNO
#define SECCOMP_RET_ERRNO 0x00050000U
#endif
#ifndef SECCOMP_RET_LOG
#define SECCOMP_RET_LOG 0x7ffc0000U
#endif
#ifndef SECCOMP_RET_ALLOW
#define SECCOMP_RET_ALLOW 0x7fff0000U
#endif
#ifndef PR_SET_NO_NEW_PRIVS
#define PR_SET_NO_NEW_PRIVS 38
#endif

#ifndef AUDIT_ARCH_X86_64
#define AUDIT_ARCH_X86_64 0xc000003eU
#endif
#ifndef AUDIT_ARCH_AARCH64
#define AUDIT_ARCH_AARCH64 0xc00000b7U
#endif
#ifndef AUDIT_ARCH_RISCV64
#define AUDIT_ARCH_RISCV64 0xc00000f3U
#endif
#ifndef AUDIT_ARCH_LOONGARCH64
#define AUDIT_ARCH_LOONGARCH64 0xc0000102U
#endif

#if defined(__x86_64__)
#define EXPECTED_AUDIT_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define EXPECTED_AUDIT_ARCH AUDIT_ARCH_AARCH64
#elif defined(__riscv)
#define EXPECTED_AUDIT_ARCH AUDIT_ARCH_RISCV64
#elif defined(__loongarch64)
#define EXPECTED_AUDIT_ARCH AUDIT_ARCH_LOONGARCH64
#else
#error "unknown test architecture"
#endif

#define SECCOMP_DATA_NR_OFF 0
#define SECCOMP_DATA_ARCH_OFF 4
#define SECCOMP_DATA_ARG0_OFF 16

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static void expect_true(int condition, const char *name)
{
    if (condition) {
        note_pass(name);
    } else {
        note_fail(name, "condition is false");
    }
}

static void expect_syscall_ret(long ret, long expected, int expected_errno,
                               const char *name)
{
    int saved_errno = errno;

    if (ret == expected && saved_errno == expected_errno) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected ret=%ld errno=%d",
             ret, saved_errno, strerror(saved_errno), expected,
             expected_errno);
    note_fail(name, detail);
}

static long seccomp_raw(unsigned int op, unsigned int flags, void *args)
{
    return syscall(SYS_seccomp, op, flags, args);
}

static int set_no_new_privs(void)
{
    errno = 0;
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0) {
        return 0;
    }
    printf("FAIL: PR_SET_NO_NEW_PRIVS: errno=%d (%s)\n", errno,
           strerror(errno));
    return -1;
}

static int install_filter(const struct sock_filter *filter, unsigned short len,
                          unsigned int flags)
{
    struct sock_fprog prog = {
        .len = len,
        .filter = (struct sock_filter *)filter,
    };

    errno = 0;
    return (int)seccomp_raw(SECCOMP_SET_MODE_FILTER, flags, &prog);
}

static void check_action_availability(void)
{
    unsigned int action;

    action = SECCOMP_RET_ALLOW;
    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, &action), 0, 0,
                       "GET_ACTION_AVAIL accepts ALLOW");

    action = SECCOMP_RET_LOG;
    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, &action), 0, 0,
                       "GET_ACTION_AVAIL accepts LOG");

    action = SECCOMP_RET_ERRNO;
    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, &action), 0, 0,
                       "GET_ACTION_AVAIL accepts ERRNO");

    action = SECCOMP_RET_KILL_THREAD;
    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, &action), 0, 0,
                       "GET_ACTION_AVAIL accepts KILL_THREAD");

    action = SECCOMP_RET_KILL_PROCESS;
    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, &action), 0, 0,
                       "GET_ACTION_AVAIL accepts KILL_PROCESS");

    action = SECCOMP_RET_TRAP;
    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, &action), -1,
                       ENOTSUP, "GET_ACTION_AVAIL rejects TRAP");
}

static void check_invalid_seccomp_args(void)
{
    unsigned int action = SECCOMP_RET_ALLOW;

    errno = 0;
    expect_syscall_ret(seccomp_raw(999U, 0, NULL), -1, EINVAL,
                       "unknown seccomp op returns EINVAL");

    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0, NULL), -1,
                       EFAULT, "GET_ACTION_AVAIL NULL pointer returns EFAULT");

    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_GET_ACTION_AVAIL, 0x80000000U,
                                   &action),
                       -1, EINVAL, "unknown seccomp flag returns EINVAL");

    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_SET_MODE_STRICT, 1, NULL), -1,
                       EINVAL, "strict mode rejects nonzero flags");

    errno = 0;
    expect_syscall_ret(seccomp_raw(SECCOMP_SET_MODE_STRICT, 0, &action), -1,
                       EINVAL, "strict mode rejects non-NULL args");
}

static void check_errno_filter(void)
{
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };

    if (set_no_new_privs() != 0) {
        failed++;
        return;
    }
    errno = 0;
    expect_syscall_ret(install_filter(filter, sizeof(filter) / sizeof(filter[0]),
                                      0),
                       0, 0, "install ERRNO filter");

    errno = 0;
    expect_syscall_ret(syscall(SYS_getpid), -1, EACCES,
                       "filter returns configured errno for getpid");

    errno = 0;
    long ret = syscall(SYS_getppid);
    expect_true(ret > 0 && errno == 0, "filter allows unrelated syscall");
}

static void check_errno_zero_maps_to_eperm(void)
{
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getuid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };

    if (set_no_new_privs() != 0) {
        failed++;
        return;
    }
    errno = 0;
    expect_syscall_ret(install_filter(filter, sizeof(filter) / sizeof(filter[0]),
                                      0),
                       0, 0, "install ERRNO zero filter");

    errno = 0;
    expect_syscall_ret(syscall(SYS_getuid), -1, EPERM,
                       "SECCOMP_RET_ERRNO data 0 maps to EPERM");
}

static void check_arch_and_arg_filter(void)
{
    int pipefd[2];

    if (pipe(pipefd) != 0) {
        note_fail("create pipe for arg filter", strerror(errno));
        return;
    }

    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARCH_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, EXPECTED_AUDIT_ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EDOM),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_close, 0, 3),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARG0_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (unsigned int)pipefd[0], 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EBUSY),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };

    if (set_no_new_privs() != 0) {
        failed++;
        close(pipefd[0]);
        close(pipefd[1]);
        return;
    }
    errno = 0;
    expect_syscall_ret(install_filter(filter, sizeof(filter) / sizeof(filter[0]),
                                      0),
                       0, 0, "install arch/arg filter");

    errno = 0;
    long ret = syscall(SYS_getppid);
    expect_true(ret > 0 && errno == 0, "filter accepts expected audit arch");

    errno = 0;
    expect_syscall_ret(syscall(SYS_close, pipefd[0]), -1, EBUSY,
                       "filter matches syscall arg0");

    errno = 0;
    expect_syscall_ret(syscall(SYS_close, pipefd[1]), 0, 0,
                       "filter allows close on other fd");
}

static void check_fork_inherits_filter(void)
{
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };

    if (set_no_new_privs() != 0) {
        failed++;
        return;
    }
    errno = 0;
    expect_syscall_ret(install_filter(filter, sizeof(filter) / sizeof(filter[0]),
                                      0),
                       0, 0, "install inherited filter");

    pid_t pid = fork();
    if (pid == 0) {
        errno = 0;
        if (syscall(SYS_getpid) == -1 && errno == EACCES) {
            _exit(0);
        }
        _exit(1);
    }

    if (pid < 0) {
        note_fail("fork after filter install", strerror(errno));
        return;
    }

    int status;
    if (waitpid(pid, &status, 0) != pid) {
        note_fail("wait child for inherited filter", strerror(errno));
        return;
    }
    expect_true(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                "child inherits seccomp filter across fork");
}

struct tsync_state {
    volatile int start;
    long ret;
    int err;
};

static void *tsync_worker(void *arg)
{
    struct tsync_state *state = (struct tsync_state *)arg;

    while (!state->start) {
        sched_yield();
    }

    errno = 0;
    state->ret = syscall(SYS_getpid);
    state->err = errno;
    return NULL;
}

static void check_tsync_filter(void)
{
    pthread_t thread;
    struct tsync_state state = {
        .start = 0,
        .ret = 0,
        .err = 0,
    };
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };

    if (pthread_create(&thread, NULL, tsync_worker, &state) != 0) {
        note_fail("create TSYNC worker", strerror(errno));
        return;
    }

    if (set_no_new_privs() != 0) {
        failed++;
        state.start = 1;
        pthread_join(thread, NULL);
        return;
    }

    errno = 0;
    expect_syscall_ret(install_filter(filter, sizeof(filter) / sizeof(filter[0]),
                                      SECCOMP_FILTER_FLAG_TSYNC),
                       0, 0, "install TSYNC filter");

    state.start = 1;
    pthread_join(thread, NULL);
    expect_true(state.ret == -1 && state.err == EACCES,
                "TSYNC applies filter to peer thread");
}

static void expect_child_killed_after_marker(pid_t pid, int read_fd,
                                             const char *name)
{
    char marker = 0;
    ssize_t n = read(read_fd, &marker, 1);
    close(read_fd);
    if (n != 1 || marker != 'R') {
        note_fail(name, "child did not reach armed seccomp state");
        if (pid > 0) {
            int ignored_status;
            waitpid(pid, &ignored_status, 0);
        }
        return;
    }

    int status;
    if (waitpid(pid, &status, 0) != pid) {
        note_fail(name, strerror(errno));
        return;
    }

    if (WIFSIGNALED(status) || (WIFEXITED(status) && WEXITSTATUS(status) != 0)) {
        note_pass(name);
    } else {
        note_fail(name, "child survived forbidden syscall");
    }
}

static void check_strict_kills_child(void)
{
    int pipefd[2];

    if (pipe(pipefd) != 0) {
        note_fail("create strict child pipe", strerror(errno));
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        close(pipefd[0]);
        if (seccomp_raw(SECCOMP_SET_MODE_STRICT, 0, NULL) != 0) {
            _exit(2);
        }
        if (write(pipefd[1], "R", 1) != 1) {
            _exit(3);
        }
        syscall(SYS_getpid);
        _exit(0);
    }

    close(pipefd[1]);
    if (pid < 0) {
        close(pipefd[0]);
        note_fail("fork strict child", strerror(errno));
        return;
    }
    expect_child_killed_after_marker(pid, pipefd[0],
                                     "strict mode kills forbidden syscall");
}

static void check_filter_kills_child(void)
{
    int pipefd[2];

    if (pipe(pipefd) != 0) {
        note_fail("create filter-kill child pipe", strerror(errno));
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        struct sock_filter filter[] = {
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_THREAD),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        };

        close(pipefd[0]);
        if (set_no_new_privs() != 0) {
            _exit(2);
        }
        if (install_filter(filter, sizeof(filter) / sizeof(filter[0]), 0) != 0) {
            _exit(3);
        }
        if (write(pipefd[1], "R", 1) != 1) {
            _exit(4);
        }
        syscall(SYS_getpid);
        _exit(0);
    }

    close(pipefd[1]);
    if (pid < 0) {
        close(pipefd[0]);
        note_fail("fork filter-kill child", strerror(errno));
        return;
    }
    expect_child_killed_after_marker(pid, pipefd[0],
                                     "filter KILL_THREAD kills child");
}

static void check_filter_kill_process_child(void)
{
    int pipefd[2];

    if (pipe(pipefd) != 0) {
        note_fail("create filter-kill-process child pipe", strerror(errno));
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        struct sock_filter filter[] = {
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        };

        close(pipefd[0]);
        if (set_no_new_privs() != 0) {
            _exit(2);
        }
        if (install_filter(filter, sizeof(filter) / sizeof(filter[0]), 0) != 0) {
            _exit(3);
        }
        if (write(pipefd[1], "R", 1) != 1) {
            _exit(4);
        }
        syscall(SYS_getpid);
        _exit(0);
    }

    close(pipefd[1]);
    if (pid < 0) {
        close(pipefd[0]);
        note_fail("fork filter-kill-process child", strerror(errno));
        return;
    }
    expect_child_killed_after_marker(pid, pipefd[0],
                                     "filter KILL_PROCESS kills child process");
}

static void check_filter_precedence_kill_over_errno(void)
{
    int pipefd[2];

    if (pipe(pipefd) != 0) {
        note_fail("create filter-precedence child pipe", strerror(errno));
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        struct sock_filter errno_filter[] = {
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        };
        struct sock_filter kill_filter[] = {
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFF),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_getpid, 0, 1),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        };

        close(pipefd[0]);
        if (set_no_new_privs() != 0) {
            _exit(2);
        }
        if (install_filter(errno_filter,
                           sizeof(errno_filter) / sizeof(errno_filter[0]),
                           0) != 0) {
            _exit(3);
        }
        if (install_filter(kill_filter,
                           sizeof(kill_filter) / sizeof(kill_filter[0]),
                           0) != 0) {
            _exit(4);
        }
        if (write(pipefd[1], "R", 1) != 1) {
            _exit(5);
        }
        syscall(SYS_getpid);
        _exit(0);
    }

    close(pipefd[1]);
    if (pid < 0) {
        close(pipefd[0]);
        note_fail("fork filter-precedence child", strerror(errno));
        return;
    }
    expect_child_killed_after_marker(pid, pipefd[0],
                                     "KILL_PROCESS takes precedence over earlier ERRNO filter");
}

static void run_isolated(void (*fn)(void), const char *name)
{
    pid_t pid = fork();

    if (pid == 0) {
        passed = 0;
        failed = 0;
        fn();
        fflush(stdout);
        _exit(failed == 0 ? 0 : 1);
    }

    if (pid < 0) {
        note_fail(name, strerror(errno));
        return;
    }

    int status;
    if (waitpid(pid, &status, 0) != pid) {
        note_fail(name, strerror(errno));
        return;
    }

    expect_true(WIFEXITED(status) && WEXITSTATUS(status) == 0, name);
}

int main(void)
{
    printf("=== syscall-test-seccomp ===\n");

    check_action_availability();
    check_invalid_seccomp_args();
    run_isolated(check_errno_filter, "ERRNO filter isolated test");
    run_isolated(check_errno_zero_maps_to_eperm, "ERRNO zero isolated test");
    run_isolated(check_arch_and_arg_filter, "arch and arg filter isolated test");
    run_isolated(check_fork_inherits_filter, "fork inheritance isolated test");
    run_isolated(check_tsync_filter, "TSYNC isolated test");
    check_strict_kills_child();
    check_filter_kills_child();
    check_filter_kill_process_child();
    check_filter_precedence_kill_over_errno();

    if (failed == 0) {
        printf("ALL PASSED\n");
        return 0;
    }

    printf("FAILED: %d passed, %d failed\n", passed, failed);
    return 1;
}
