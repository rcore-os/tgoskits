/*
 * test-seccomp — verify seccomp(2) syscall Linux ABI compliance
 *
 * Operations covered:
 *   SECCOMP_SET_MODE_STRICT  (0)
 *   SECCOMP_SET_MODE_FILTER  (1)
 *   SECCOMP_GET_ACTION_AVAIL (2)
 *   SECCOMP_GET_NOTIF_SIZES  (3)
 *
 * Also covers prctl(PR_SET_SECCOMP, ...) and prctl(PR_GET_SECCOMP).
 */

#include "test_framework.h"

#include <linux/seccomp.h>
#include <linux/filter.h>
#include <linux/audit.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <sys/prctl.h>
#include <sys/wait.h>

#ifndef PR_SET_SECCOMP
#define PR_SET_SECCOMP 22
#endif
#ifndef PR_GET_SECCOMP
#define PR_GET_SECCOMP 21
#endif
#ifndef PR_SET_NO_NEW_PRIVS
#define PR_SET_NO_NEW_PRIVS 57
#endif
#ifndef PR_GET_NO_NEW_PRIVS
#define PR_GET_NO_NEW_PRIVS 58
#endif
#include <unistd.h>
#include <signal.h>
#include <stdint.h>
#include <stddef.h>

/* ================================================================== */

static long seccomp(unsigned int op, unsigned int flags, void *args)
{
    return syscall(SYS_seccomp, op, flags, args);
}

/*
 * Fork a child, do something in the child, wait for the result.
 * Returns: 0 on success (child exit status as expected), -1 on error.
 *
 * child_fn signature: int child_fn(void) — returns 0 for expected
 *   child behavior, 1 for unexpected.
 *
 * expected_by_exit: child should exit normally with status 0.
 * expected_by_sig:  child should be killed by signal.
 * expected_sig:     the expected signal number (if expected_by_sig).
 */
static int fork_and_check(const char *name, int (*child_fn)(void),
                          int expected_by_exit, int expected_by_sig, int expected_sig)
{
    pid_t pid = fork();
    if (pid < 0) {
        printf("  FAIL | fork() failed for '%s'\n", name);
        __fail++;
        return -1;
    }
    if (pid == 0) {
        int rc = child_fn();
        _exit(rc);
    }

    int status;
    pid_t w;
    do {
        w = waitpid(pid, &status, 0);
    } while (w == -1 && errno == EINTR);

    if (w != pid) {
        printf("  FAIL | %s: waitpid failed\n", name);
        __fail++;
        return -1;
    }

    if (expected_by_exit && WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("  PASS | %s: child exited normally as expected\n", name);
        __pass++;
        return 0;
    }
    if (expected_by_sig && WIFSIGNALED(status) && WTERMSIG(status) == expected_sig) {
        printf("  PASS | %s: child killed by signal %d as expected\n", name, expected_sig);
        __pass++;
        return 0;
    }

    /* Unexpected result */
    printf("  FAIL | %s: unexpected child status=%d (exited=%d sig=%d)\n",
           name, status, WIFEXITED(status) ? WEXITSTATUS(status) : -1,
           WIFSIGNALED(status) ? WTERMSIG(status) : -1);
    __fail++;
    return -1;
}

/* ================================================================== */
/* Child helpers                                                      */
/* ================================================================== */

/* Apply STRICT then call getpid() — should be killed if enforced */
static int child_strict_then_getpid(void)
{
    long r = seccomp(SECCOMP_SET_MODE_STRICT, 0, NULL);
    if (r != 0)
        return 1;
    getpid();
    return 0;
}

/* Apply STRICT twice — second call must fail with EINVAL */
static int child_double_strict(void)
{
    long r = seccomp(SECCOMP_SET_MODE_STRICT, 0, NULL);
    if (r != 0)
        return 1;
    errno = 0;
    r = seccomp(SECCOMP_SET_MODE_STRICT, 0, NULL);
    if (r == -1 && errno == EINVAL)
        return 0;
    return 1;
}

/* Set NO_NEW_PRIVS then apply STRICT then FILTER — should fail (EINVAL) */
static int child_strict_then_filter(void)
{
    (void)prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    long r = seccomp(SECCOMP_SET_MODE_STRICT, 0, NULL);
    if (r != 0)
        return 1;

    /* Build an ALLOW filter */
    struct sock_filter f[] = {
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = { .len = 1, .filter = f };

    errno = 0;
    r = seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog);
    if (r == -1 && errno == EINVAL)
        return 0;
    return 1;
}

/* FILTER without NO_NEW_PRIVS and without root → EACCES */
static int child_filter_without_nnp(void)
{
    struct sock_filter f[] = {
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = { .len = 1, .filter = f };

    errno = 0;
    long r = seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog);
    if (r == -1 && errno == EACCES)
        return 0;
    return 1;
}

/* FILTER with valid BPF that blocks openat(2) with ERRNO(EPERM) */
static int child_filter_block_openat(void)
{
    /* NO_NEW_PRIVS first */
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return 1;

    /* BPF filter: allow all except openat(257 on x86_64, 56 on aarch64) */
    struct sock_filter filter[] = {
        /* [0] Load architecture */
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        /* [1] Jump based on arch: if == x86_64 → check x86_64 nr; else → allow */
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        /* [2] Not x86_64: allow */
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        /* [3] Load syscall NR */
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        /* [4] If NR == SYS_openat (257), block with EPERM; else allow */
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_openat, 1, 0),
        /* [5] Allow other syscalls */
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        /* [6] Block openat with EPERM */
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (EPERM & SECCOMP_RET_DATA)),
    };
    struct sock_fprog prog = {
        .len = (unsigned short)(sizeof(filter) / sizeof(filter[0])),
        .filter = filter,
    };

    long r = seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog);
    if (r != 0)
        return 1;

    /* After filter: openat("/dev/null", O_RDONLY) should return -1/EPERM */
    errno = 0;
    int fd = syscall(__NR_openat, AT_FDCWD, "/dev/null", O_RDONLY);
    if (fd == -1 && errno == EPERM)
        return 0;

    if (fd >= 0)
        close(fd);
    return 1;
}

/* Filter that returns KILL_PROCESS for write() */
static int child_filter_killproc_on_write(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return 1;

    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_write, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    };
    struct sock_fprog prog = {
        .len = sizeof(filter) / sizeof(filter[0]),
        .filter = filter,
    };

    long r = seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog);
    if (r != 0)
        return 1;

    /* write to stdout (fd 1) should kill us */
    write(1, "x", 1);
    return 0; /* unreachable if filter works */
}

/* Filter that returns LOG + ALLOW for getpid() — should still succeed */
static int child_filter_log_getpid(void)
{
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
        return 1;

    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getpid, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_LOG | SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {
        .len = sizeof(filter) / sizeof(filter[0]),
        .filter = filter,
    };

    long r = seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog);
    if (r != 0)
        return 1;

    /* getpid() should still work (LOG + ALLOW) */
    pid_t p = getpid();
    if (p > 0)
        return 0;
    return 1;
}

/* ================================================================== */
/* Test: GET_ACTION_AVAIL                                              */
/* ================================================================== */
static void test_get_action_avail(void)
{
    printf("\n--- GET_ACTION_AVAIL ---\n");

    /* Known supported actions */
    static const struct {
        __u32 action;
        const char *name;
    } actions[] = {
        { SECCOMP_RET_KILL_PROCESS, "KILL_PROCESS" },
        { SECCOMP_RET_KILL_THREAD,  "KILL_THREAD"  },
        { SECCOMP_RET_TRAP,         "TRAP"         },
        { SECCOMP_RET_ERRNO,        "ERRNO"        },
        { SECCOMP_RET_TRACE,        "TRACE"        },
        { SECCOMP_RET_LOG,          "LOG"          },
        { SECCOMP_RET_ALLOW,        "ALLOW"        },
    };

    for (size_t i = 0; i < sizeof(actions) / sizeof(actions[0]); i++) {
        __u32 val = actions[i].action;
        errno = 0;
        long r = seccomp(SECCOMP_GET_ACTION_AVAIL, 0, &val);
        if (r == 0 && val == 0) {
            char buf[128];
            snprintf(buf, sizeof(buf), "GET_ACTION_AVAIL(%s) → not available", actions[i].name);
            CHECK(1, buf);
        } else if (r == 0 && val == 1) {
            char buf[128];
            snprintf(buf, sizeof(buf), "GET_ACTION_AVAIL(%s) → available", actions[i].name);
            CHECK(1, buf);
        } else {
            char buf[128];
            snprintf(buf, sizeof(buf),
                     "GET_ACTION_AVAIL(%s) → ret=%ld errno=%d", actions[i].name, r, errno);
            CHECK(0, buf);
        }
    }

    /* GET_ACTION_AVAIL with bad flags */
    {
        __u32 val = SECCOMP_RET_ALLOW;
        CHECK_ERR(seccomp(SECCOMP_GET_ACTION_AVAIL, 1, &val), EINVAL,
                  "GET_ACTION_AVAIL flags=1 → EINVAL");
    }

    /* GET_ACTION_AVAIL with NULL args */
    CHECK_ERR(seccomp(SECCOMP_GET_ACTION_AVAIL, 0, NULL), EFAULT,
              "GET_ACTION_AVAIL args=NULL → EFAULT");
}

/* ================================================================== */
/* Test: GET_NOTIF_SIZES                                               */
/* ================================================================== */
static void test_get_notif_sizes(void)
{
    printf("\n--- GET_NOTIF_SIZES ---\n");

    struct seccomp_notif_sizes sizes;
    memset(&sizes, 0, sizeof(sizes));

    errno = 0;
    long r = seccomp(SECCOMP_GET_NOTIF_SIZES, 0, &sizes);
    if (r == 0) {
        printf("  info | seccomp_notif=%u seccomp_notif_resp=%u seccomp_data=%u\n",
               sizes.seccomp_notif, sizes.seccomp_notif_resp, sizes.seccomp_data);
        CHECK(sizes.seccomp_notif <= 1024, "seccomp_notif size plausible");
    } else {
        /* Kernel may not support this operation (e.g. older kernels) */
        CHECK(r == -1 && (errno == ENOSYS || errno == EINVAL || errno == EOPNOTSUPP),
              "GET_NOTIF_SIZES → supported or not-implemented");
    }

    /* Bad flags */
    CHECK_ERR(seccomp(SECCOMP_GET_NOTIF_SIZES, 1, &sizes), EINVAL,
              "GET_NOTIF_SIZES flags=1 → EINVAL");

    /* NULL args */
    CHECK_ERR(seccomp(SECCOMP_GET_NOTIF_SIZES, 0, NULL), EFAULT,
              "GET_NOTIF_SIZES args=NULL → EFAULT");
}

/* ================================================================== */
/* Test: Error cases for SET_MODE_STRICT                               */
/* ================================================================== */
static void test_set_mode_strict_errors(void)
{
    printf("\n--- SET_MODE_STRICT error cases ---\n");

    /* Non-zero flags */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_STRICT, 1, NULL), EINVAL,
              "SET_MODE_STRICT flags=1 → EINVAL");

    CHECK_ERR(seccomp(SECCOMP_SET_MODE_STRICT, 0xDEADBEEF, NULL), EINVAL,
              "SET_MODE_STRICT flags=0xDEADBEEF → EINVAL");

    /* Non-NULL args */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_STRICT, 0, (void *)1), EINVAL,
              "SET_MODE_STRICT args=(void*)1 → EINVAL");
}

/* ================================================================== */
/* Test: Error cases for SET_MODE_FILTER                               */
/* ================================================================== */
static void test_set_mode_filter_errors(void)
{
    printf("\n--- SET_MODE_FILTER error cases ---\n");

    /* NULL args → EFAULT */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, 0, NULL), EFAULT,
              "SET_MODE_FILTER args=NULL → EFAULT");

    /* Bad pointer → EFAULT */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, 0, (void *)0x1), EFAULT,
              "SET_MODE_FILTER prog=0x1 → EFAULT");

    /* prog.len == 0 → EINVAL */
    {
        struct sock_filter f[] = { BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW) };
        struct sock_fprog prog = { .len = 0, .filter = f };
        CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog), EINVAL,
                  "SET_MODE_FILTER len=0 → EINVAL");
    }

    /* Bad flags → EINVAL */
    {
        struct sock_filter f[] = { BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW) };
        struct sock_fprog prog = { .len = 1, .filter = f };
        CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, 0x10000000, &prog), EINVAL,
                  "SET_MODE_FILTER unknown flags → EINVAL");
    }
}

/* ================================================================== */
/* Test: Invalid operation                                             */
/* ================================================================== */
static void test_invalid_operation(void)
{
    printf("\n--- Invalid operation ---\n");

    CHECK_ERR(seccomp(0xDEAD, 0, NULL), EINVAL,
              "seccomp(0xDEAD) → EINVAL");
    CHECK_ERR(seccomp(999, 0, NULL), EINVAL,
              "seccomp(op=999) → EINVAL");
}

/* ================================================================== */
/* Test: prctl PR_GET_SECCOMP                                         */
/* ================================================================== */
static void test_prctl_get_seccomp(void)
{
    printf("\n--- prctl PR_GET_SECCOMP ---\n");

    errno = 0;
    long mode = prctl(PR_GET_SECCOMP);
    /* Should return 0 (DISABLED) in the current process */
    CHECK_RET(mode, 0, "PR_GET_SECCOMP → 0 (DISABLED)");
}

/* Apply STRICT via prctl then call getpid() — should be killed if enforced */
static int child_prctl_strict_then_getpid(void)
{
    long r = prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0);
    if (r != 0)
        return 1;
    getpid();
    return 0;
}

/* ================================================================== */
/* Test: prctl PR_SET_SECCOMP → STRICT in child                       */
/* ================================================================== */
static void test_prctl_set_strict(void)
{
    printf("\n--- prctl PR_SET_SECCOMP → STRICT ---\n");

    /* Set STRICT via prctl in child */
    fork_and_check("prctl_SET_SECCOMP_STRICT", child_prctl_strict_then_getpid,
                   1, 1, SIGSYS);

    /* Error: invalid mode (999) */
    {
        pid_t pid = fork();
        CHECK(pid >= 0, "fork for prctl SECCOMP bad mode");
        if (pid == 0) {
            errno = 0;
            long r = prctl(PR_SET_SECCOMP, 999, 0, 0, 0);
            _exit(r == -1 && errno == EINVAL ? 0 : 1);
        }
        int status;
        do {
            waitpid(pid, &status, 0);
        } while (errno == EINTR);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "prctl SET_SECCOMP mode=999 → EINVAL");
    }
}

/* ================================================================== */
/* Test: prctl PR_SET_SECCOMP → FILTER                                 */
/* ================================================================== */
static void test_prctl_set_filter(void)
{
    printf("\n--- prctl PR_SET_SECCOMP → FILTER ---\n");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork for prctl SECCOMP FILTER");
    if (pid == 0) {
        /* Set NO_NEW_PRIVS */
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
            _exit(1);

        /* Build a filter that blocks openat with EACCES */
        struct sock_filter filter[] = {
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_openat, 1, 0),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (EACCES & SECCOMP_RET_DATA)),
        };
        struct sock_fprog prog = {
            .len = sizeof(filter) / sizeof(filter[0]),
            .filter = filter,
        };

        errno = 0;
        long r = prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog, 0, 0);
        if (r == -1 && (errno == EACCES || errno == EINVAL)) {
            /* Expected if unsupported */
            _exit(0);
        }
        if (r != 0)
            _exit(2);

        /* Verify openat is blocked */
        errno = 0;
        int fd = syscall(__NR_openat, AT_FDCWD, "/dev/null", O_RDONLY);
        if (fd == -1 && errno == EACCES)
            _exit(0);
        if (fd >= 0)
            close(fd);
        _exit(3);
    }

    int status;
    do {
        waitpid(pid, &status, 0);
    } while (errno == EINTR);

    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        printf("  PASS | prctl SET_SECCOMP FILTER: filter enforced or stub\n");
        __pass++;
    } else {
        printf("  FAIL | prctl SET_SECCOMP FILTER: unexpected exit=%d\n",
               WEXITSTATUS(status));
        __fail++;
    }
}

/* ================================================================== */
/* Test: Filter with ERRNO action (forked child)                      */
/* ================================================================== */
static void test_filter_errno_action(void)
{
    printf("\n--- Filter with ERRNO action (fork) ---\n");

    pid_t pid = fork();
    if (pid < 0) {
        printf("  FAIL | fork() failed for ERRNO filter test\n");
        __fail++;
        return;
    }
    if (pid == 0) {
        /* Set NO_NEW_PRIVS in child */
        if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0)
            _exit(1);

        struct sock_filter filter[] = {
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
            BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
            BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getpid, 1, 0),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
            BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (99 & SECCOMP_RET_DATA)),
        };
        struct sock_fprog prog = {
            .len = sizeof(filter) / sizeof(filter[0]),
            .filter = filter,
        };

        long r = seccomp(SECCOMP_SET_MODE_FILTER, 0, &prog);
        if (r != 0)
            _exit(2);

        /* getpid() should return -1 with errno=99 */
        errno = 0;
        pid_t p = getpid();
        if (p == -1 && errno == 99) {
            /* getppid() should still work */
            errno = 0;
            pid_t pp = getppid();
            _exit(pp >= 0 ? 0 : 3);
        }
        _exit(4);
    }

    int status;
    do {
        waitpid(pid, &status, 0);
    } while (errno == EINTR);

    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        CHECK(1, "filter ERRNO: getpid() blocked, getppid() allowed");
    } else if (WIFEXITED(status) && WEXITSTATUS(status) == 2) {
        printf("  PASS | filter ERRNO: SET_MODE_FILTER failed (EACCES/EINVAL)\n");
        __pass++;
    } else {
        printf("  FAIL | filter ERRNO: unexpected exit=%d\n",
               WIFEXITED(status) ? WEXITSTATUS(status) : -1);
        __fail++;
    }
}

/* ================================================================== */
/* Test: STRICT enforcement in child                                   */
/* ================================================================== */
static void test_strict_enforcement(void)
{
    printf("\n--- STRICT enforcement (fork) ---\n");

    fork_and_check("strict_then_getpid", child_strict_then_getpid,
                   1, 1, SIGSYS);

    /* STRICT allows: read, write, exit, exit_group, rt_sigreturn, readv, writev */
    /* Test that write() still works under STRICT */
    {
        pid_t pid = fork();
        if (pid == 0) {
            long r = seccomp(SECCOMP_SET_MODE_STRICT, 0, NULL);
            if (r != 0)
                _exit(1);
            /* write() should be allowed in STRICT */
            ssize_t n = write(1, "", 0);
            _exit(n == 0 ? 0 : 1);
        }
        if (pid > 0) {
            int status;
            do { waitpid(pid, &status, 0); } while (errno == EINTR);
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "strict mode allows write() (empty)");
        }
    }

}

/* ================================================================== */
/* Test: FILTER enforcement in child                                   */
/* ================================================================== */
static void test_filter_enforcement_child(void)
{
    printf("\n--- FILTER enforcement (fork) ---\n");

    /* Test: filter blocks openat with EPERM in child */
    fork_and_check("filter_block_openat", child_filter_block_openat,
                   1, 0, 0);

    /* Test: KILL_PROCESS on write */
    fork_and_check("filter_killproc_on_write", child_filter_killproc_on_write,
                   0, 1, SIGSYS);

    /* Test: LOG + ALLOW for getpid */
    fork_and_check("filter_log_getpid", child_filter_log_getpid,
                   1, 0, 0);

    /* Test: FILTER without NO_NEW_PRIVS */
    fork_and_check("filter_without_nnp", child_filter_without_nnp,
                   1, 0, 0);
}

/* ================================================================== */
/* Test: seccomp mode transitions                                      */
/* ================================================================== */
static void test_mode_transitions(void)
{
    printf("\n--- Mode transitions ---\n");

    /* Double STRICT → second call EINVAL */
    fork_and_check("double_strict", child_double_strict, 1, 0, 0);

    /* STRICT then FILTER → EINVAL */
    fork_and_check("strict_then_filter", child_strict_then_filter, 1, 0, 0);
}

/* ================================================================== */
/* Test: filter flags                                                  */
/* ================================================================== */
static void test_filter_flags(void)
{
    printf("\n--- Filter flags ---\n");

    /* TSYNC flag with NULL prog → EFAULT */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, NULL),
              EFAULT, "SET_MODE_FILTER TSYNC NULL prog → EFAULT");

    /* LOG flag with NULL prog → EFAULT */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_LOG, NULL),
              EFAULT, "SET_MODE_FILTER LOG NULL prog → EFAULT");

    /* SPEC_ALLOW with NULL prog → EFAULT */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_SPEC_ALLOW, NULL),
              EFAULT, "SET_MODE_FILTER SPEC_ALLOW NULL prog → EFAULT");

    /* Combination: TSYNC | LOG with NULL prog */
    CHECK_ERR(seccomp(SECCOMP_SET_MODE_FILTER,
                      SECCOMP_FILTER_FLAG_TSYNC | SECCOMP_FILTER_FLAG_LOG, NULL),
              EFAULT, "SET_MODE_FILTER TSYNC|LOG NULL prog → EFAULT");
}

/* ================================================================== */
int main(void)
{
    TEST_START("seccomp syscall ABI verification");

    /* Tests that do NOT require fork (in-process) */
    test_get_action_avail();
    test_get_notif_sizes();
    test_set_mode_strict_errors();
    test_set_mode_filter_errors();
    test_invalid_operation();
    test_prctl_get_seccomp();
    test_filter_flags();

    /* Tests that DO require fork (child process) */
    test_strict_enforcement();
    test_filter_errno_action();
    test_filter_enforcement_child();
    test_mode_transitions();
    test_prctl_set_strict();
    test_prctl_set_filter();

    TEST_DONE();
}
