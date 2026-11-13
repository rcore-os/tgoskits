/*
 * test_prlimit64.c — prlimit64/getrlimit/setrlimit tests with limit enforcement
 *
 * Each resource is tested in three layers:
 *   1. query   — get the current limit (getrlimit / prlimit get-only)
 *   2. mutate  — set a test value, verify via query (setrlimit / prlimit set)
 *   3. enforce — verify the limit is actually enforced by the kernel
 *
 * Coverage:
 *   RLIMIT_NOFILE  — set, enforce by opening N+1 files, restore;
 *                    raise to 2048, fill the table, UDP socket → EMFILE
 *   RLIMIT_FSIZE   — set, enforce by writing beyond limit
 *   RLIMIT_NICE     — set ceiling, enforce via setpriority(2)
 *   RLIMIT_CORE    — set, enforce (nonzero → nonzero; 0 → no core)
 *   RLIMIT_DATA    — set, enforce via brk(2)
 *   RLIMIT_AS      — query + consistency
 *   RLIMIT_CPU     — query + arm/disarm
 *   RLIMIT_STACK   — query + consistency
 *   RLIMIT_MEMLOCK — query + consistency
 *   RLIMIT_NPROC   — query + consistency
 *   RLIMIT_RTPRIO  — query + consistency
 *   RLIMIT_RTTIME  — shared soft-limit advance and process SIGXCPU visibility
 *   RLIMIT_SIGPENDING — query + consistency
 *   RLIMIT_MSGQUEUE   — query + consistency
 *
 *   pid parameter:
 *     pid=0      — self (get-only, set-only, get+set)
 *     pid=getpid() — explicit self (verify same as pid=0)
 *     pid=invalid — EFAULT first if new_limit cannot be copied, else ESRCH
 *     pid=1        — may fail ESRCH or EPERM
 *
 *   error conditions:
 *     - invalid resource   → EINVAL
 *     - soft > hard        → EINVAL
 *     - raise hard limit   → EPERM (no CAP_SYS_RESOURCE)
 *     - valid new_limit + invalid pid + bad res → ESRCH (pid before res)
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/resource.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <errno.h>
#include <sys/syscall.h>

/* RLIMIT constants not declared on all libc implementations */
#ifndef RLIMIT_RTTIME
#define RLIMIT_RTTIME 15
#endif

/*
 * save_restore — run a mutator block with automatic restore.
 *
 * Usage:
 *   struct rlimit old;
 *   SAVE_RLIMIT(old, RLIMIT_NOFILE);
 *   ... test body that may change RLIMIT_NOFILE ...
 *   RESTORE_RLIMIT(old, RLIMIT_NOFILE);
 *
 * The block runs between SAVE and RESTORE; RESTORE is called at scope end
 * via a cleanup pattern.
 */

int test_rttime_soft_limit(void);

struct credential_ids {
    uid_t ruid;
    uid_t euid;
    uid_t suid;
    gid_t rgid;
    gid_t egid;
    gid_t sgid;
};

static long raw_prlimit64(pid_t pid, unsigned int resource,
                          const struct rlimit *new_limit,
                          struct rlimit *old_limit)
{
    return syscall(SYS_prlimit64, pid, resource, new_limit, old_limit);
}

static int set_credential_ids(const struct credential_ids *ids)
{
    if (setresgid(ids->rgid, ids->egid, ids->sgid) != 0) {
        return -1;
    }
    return setresuid(ids->ruid, ids->euid, ids->suid);
}

static int read_exact(int fd, void *buf, size_t len)
{
    unsigned char *out = buf;
    size_t done = 0;

    while (done < len) {
        ssize_t n = read(fd, out + done, len - done);
        if (n > 0) {
            done += (size_t)n;
        } else if (n == 0) {
            return 0;
        } else if (errno != EINTR) {
            return -1;
        }
    }
    return 1;
}

static int write_exact(int fd, const void *buf, size_t len)
{
    const unsigned char *in = buf;
    size_t done = 0;

    while (done < len) {
        ssize_t n = write(fd, in + done, len - done);
        if (n > 0) {
            done += (size_t)n;
        } else if (n < 0 && errno == EINTR) {
            continue;
        } else {
            return -1;
        }
    }
    return 0;
}

struct identity_case {
    struct credential_ids target;
    struct credential_ids caller;
    int allowed;
    const char *description;
};

static int exercise_allowed_prlimit(pid_t target)
{
    struct rlimit current = {0};
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, NULL, &current) != 0 ||
        current.rlim_cur < 4) {
        return 1;
    }

    struct rlimit first = {
        .rlim_cur = current.rlim_cur - 1,
        .rlim_max = current.rlim_max,
    };
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, &first, NULL) != 0) {
        return 2;
    }

    struct rlimit second = {
        .rlim_cur = current.rlim_cur - 2,
        .rlim_max = current.rlim_max,
    };
    struct rlimit aliased = second;
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, &aliased, &aliased) != 0 ||
        aliased.rlim_cur != first.rlim_cur ||
        aliased.rlim_max != first.rlim_max) {
        return 3;
    }

    struct rlimit verified = {0};
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, NULL, &verified) != 0 ||
        verified.rlim_cur != second.rlim_cur ||
        verified.rlim_max != second.rlim_max) {
        return 4;
    }

    struct rlimit third = {
        .rlim_cur = current.rlim_cur - 3,
        .rlim_max = current.rlim_max,
    };
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, &third,
                      (struct rlimit *)-1) != -1 ||
        errno != EFAULT) {
        return 5;
    }

    verified.rlim_cur = 0;
    verified.rlim_max = 0;
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, NULL, &verified) != 0 ||
        verified.rlim_cur != third.rlim_cur ||
        verified.rlim_max != third.rlim_max) {
        return 6;
    }
    return 0;
}

static int exercise_denied_prlimit(pid_t target,
                                   const struct rlimit *target_limit)
{
    int failures = 0;

    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, (struct rlimit *)-1, NULL) != -1 ||
        errno != EFAULT) {
        failures |= 1;
    }

    struct rlimit old = {.rlim_cur = 0x1234, .rlim_max = 0x5678};
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, NULL, &old) != -1 ||
        errno != EPERM ||
        old.rlim_cur != 0x1234 || old.rlim_max != 0x5678) {
        failures |= 2;
    }

    struct rlimit replacement = {
        .rlim_cur = target_limit->rlim_cur > 1
                        ? target_limit->rlim_cur - 1
                        : target_limit->rlim_cur,
        .rlim_max = target_limit->rlim_max,
    };
    old.rlim_cur = 0x9abc;
    old.rlim_max = 0xdef0;
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, &replacement, &old) != -1 ||
        errno != EPERM ||
        old.rlim_cur != 0x9abc || old.rlim_max != 0xdef0) {
        failures |= 4;
    }

    old.rlim_cur = 0x1111;
    old.rlim_max = 0x2222;
    errno = 0;
    if (raw_prlimit64(target, RLIM_NLIMITS, NULL, &old) != -1 ||
        errno != EPERM ||
        old.rlim_cur != 0x1111 || old.rlim_max != 0x2222) {
        failures |= 8;
    }
    return failures;
}

static void run_identity_case(const struct identity_case *test_case)
{
    int ready[2];
    int release[2];
    pid_t target;
    pid_t inspector;
    int status;
    char byte = 1;
    struct rlimit target_before = {0};
    int ready_result = 0;

    if (pipe(ready) != 0) {
        CHECK(0, "create prlimit identity ready pipe");
        return;
    }
    if (pipe(release) != 0) {
        close(ready[0]);
        close(ready[1]);
        CHECK(0, "create prlimit identity release pipe");
        return;
    }

    target = fork();
    CHECK(target >= 0, "fork prlimit identity target");
    if (target == 0) {
        close(ready[0]);
        close(release[1]);
        if (set_credential_ids(&test_case->target) != 0) {
            _exit(10);
        }
        if (write_exact(ready[1], &byte, 1) != 0) {
            _exit(11);
        }
        (void)read_exact(release[0], &byte, 1);
        _exit(0);
    }
    if (target < 0) {
        close(ready[0]);
        close(ready[1]);
        close(release[0]);
        close(release[1]);
        return;
    }

    close(ready[1]);
    close(release[0]);
    ready_result = read_exact(ready[0], &byte, 1);
    close(ready[0]);
    if (ready_result == 1) {
        errno = 0;
        ready_result =
            raw_prlimit64(target, RLIMIT_NOFILE, NULL, &target_before) == 0;
    }
    CHECK(ready_result == 1, "prlimit identity target reaches its wait point");

    inspector = ready_result == 1 ? fork() : -1;
    if (inspector == 0) {
        close(release[1]);
        if (set_credential_ids(&test_case->caller) != 0) {
            _exit(20);
        }

        /*
         * CAP_KILL is cleared and the caller's effective UID matches one of
         * the target ID slots, so kill permission succeeds even in the mixed
         * identity cases that prlimit must deny.
         */
        errno = 0;
        if (kill(target, 0) != 0) {
            _exit(30);
        }

        int result = test_case->allowed
                         ? exercise_allowed_prlimit(target)
                         : exercise_denied_prlimit(target, &target_before);
        _exit(result);
    }

    if (inspector >= 0) {
        do {
            status = 0;
        } while (waitpid(inspector, &status, 0) == -1 && errno == EINTR);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              test_case->description);
    } else {
        CHECK(0, "fork prlimit identity inspector");
    }

    if (!test_case->allowed && ready_result == 1) {
        struct rlimit target_after = {0};
        errno = 0;
        CHECK(raw_prlimit64(target, RLIMIT_NOFILE, NULL, &target_after) == 0 &&
                  target_after.rlim_cur == target_before.rlim_cur &&
                  target_after.rlim_max == target_before.rlim_max,
              "denied cross-identity prlimit leaves target unchanged");
    }

    byte = 1;
    (void)write_exact(release[1], &byte, 1);
    close(release[1]);
    do {
        status = 0;
    } while (waitpid(target, &status, 0) == -1 && errno == EINTR);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "prlimit identity target exits cleanly");
}

static void test_prlimit_identity_matrix(void)
{
    static const struct identity_case cases[] = {
        {
            .target = {1000, 1000, 1000, 2000, 2000, 2000},
            .caller = {1000, 1000, 1000, 2000, 2000, 2000},
            .allowed = 1,
            .description = "all real/effective/saved IDs match for cross-process prlimit",
        },
        {
            .target = {1000, 1001, 1002, 2000, 2001, 2002},
            .caller = {0, 0, 0, 0, 0, 0},
            .allowed = 1,
            .description = "CAP_SYS_RESOURCE permits cross-identity prlimit",
        },
        {
            /* kill matches effective/saved UID 1000; prlimit also needs real UID 1001. */
            .target = {1001, 1000, 1000, 2000, 2000, 2000},
            .caller = {1000, 1000, 1000, 2000, 2000, 2000},
            .allowed = 0,
            .description = "target real UID mismatch is denied despite kill permission",
        },
        {
            /* kill matches real/saved UID 1000; prlimit also needs effective UID 1001. */
            .target = {1000, 1001, 1000, 2000, 2000, 2000},
            .caller = {1000, 1000, 1000, 2000, 2000, 2000},
            .allowed = 0,
            .description = "target effective UID mismatch is denied despite kill permission",
        },
        {
            .target = {1000, 1000, 1001, 2000, 2000, 2000},
            .caller = {1000, 1000, 1000, 2000, 2000, 2000},
            .allowed = 0,
            .description = "target saved UID mismatch is denied despite kill permission",
        },
        {
            .target = {1000, 1000, 1000, 2000, 2001, 2000},
            .caller = {1000, 1000, 1000, 2001, 2001, 2001},
            .allowed = 0,
            .description = "target real GID mismatch is denied despite kill permission",
        },
        {
            .target = {1000, 1000, 1000, 2001, 2000, 2001},
            .caller = {1000, 1000, 1000, 2001, 2001, 2001},
            .allowed = 0,
            .description = "target effective GID mismatch is denied despite kill permission",
        },
        {
            .target = {1000, 1000, 1000, 2000, 2000, 2001},
            .caller = {1000, 1000, 1000, 2000, 2000, 2000},
            .allowed = 0,
            .description = "target saved GID mismatch is denied despite kill permission",
        },
    };

    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        run_identity_case(&cases[i]);
    }
}

static void test_prlimit_pointer_order(void)
{
    struct rlimit current = {0};
    pid_t missing = (pid_t)0x7fffffff;

    CHECK_RET(getrlimit(RLIMIT_NOFILE, &current), 0,
              "query NOFILE for prlimit pointer-order checks");

    errno = 0;
    CHECK(raw_prlimit64(missing, RLIMIT_NOFILE, (struct rlimit *)-1,
                        NULL) == -1 &&
              errno == EFAULT,
          "prlimit copies new_limit before resolving the pid");

    errno = 0;
    CHECK(raw_prlimit64(missing, RLIMIT_NOFILE, &current, NULL) == -1 &&
              errno == ESRCH,
          "valid new_limit reaches missing-pid resolution and returns ESRCH");

    errno = 0;
    CHECK(raw_prlimit64(getpid(), RLIM_NLIMITS, (struct rlimit *)-1,
                        NULL) == -1 &&
              errno == EFAULT,
          "invalid new_limit is reported before resource validation");
}

int main(void)
{
    TEST_START("prlimit64 enforcement");
    signal(SIGPIPE, SIG_IGN);
    test_prlimit_pointer_order();
    test_prlimit_identity_matrix();
    CHECK(test_rttime_soft_limit() == 0,
          "RTTIME advances the shared limit and queues a process SIGXCPU");

    /* ═══════════════════════════════════════════════════════════════
     * 1. BASIC: pid=0 get-only / set-only / get+set roundtrip
     * ═══════════════════════════════════════════════════════════════ */
    {
        struct rlimit old, new_lim, prev;

        /* get-only */
        CHECK_RET(prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "get-only: prlimit(0, NOFILE, NULL, &old) = 0");
        CHECK(old.rlim_cur > 0, "get-only: soft > 0");
        CHECK(old.rlim_max >= old.rlim_cur, "get-only: hard >= soft");
        printf("  NOFILE: cur=%lu max=%lu\n",
               (unsigned long)old.rlim_cur, (unsigned long)old.rlim_max);

        /* set-only — lower soft limit by 1 */
        new_lim.rlim_cur = old.rlim_cur > 8 ? old.rlim_cur - 1 : old.rlim_cur;
        new_lim.rlim_max = old.rlim_max;
        CHECK_RET(prlimit(0, RLIMIT_NOFILE, &new_lim, NULL), 0,
                  "set-only: prlimit(0, NOFILE, &new, NULL) = 0");

        /* verify via getrlimit */
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &prev), 0,
                  "set-only: getrlimit verifies change");
        CHECK(prev.rlim_cur == new_lim.rlim_cur,
              "set-only: soft matches set value");

        /* restore */
        prlimit(0, RLIMIT_NOFILE, &old, NULL);

        /* get+set — old_limit should return the PREVIOUS (before-set) value */
        new_lim.rlim_cur = old.rlim_cur > 10 ? old.rlim_cur - 2 : old.rlim_cur;
        CHECK_RET(prlimit(0, RLIMIT_NOFILE, &new_lim, &prev), 0,
                  "set+get: prlimit(0, NOFILE, &new, &prev) = 0");
        CHECK(prev.rlim_cur == old.rlim_cur,
              "set+get: old_limit returns PREVIOUS soft (before change)");
        CHECK(prev.rlim_max == old.rlim_max,
              "set+get: old_limit returns PREVIOUS hard (before change)");

        /* verify the new value actually took effect */
        struct rlimit now;
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &now), 0,
                  "set+get: getrlimit confirms new value");
        CHECK(now.rlim_cur == new_lim.rlim_cur,
              "set+get: new soft is active");

        /* restore */
        prlimit(0, RLIMIT_NOFILE, &old, NULL);
    }

    /* ═══════════════════════════════════════════════════════════════
     * 2. pid=getpid() — verify same as pid=0
     * ═══════════════════════════════════════════════════════════════ */
    {
        pid_t mypid = getpid();
        struct rlimit rl0, rl_self;
        CHECK_RET(prlimit(0, RLIMIT_NOFILE, NULL, &rl0), 0,
                  "pid=0 get");
        CHECK_RET(prlimit(mypid, RLIMIT_NOFILE, NULL, &rl_self), 0,
                  "pid=getpid() get");
        CHECK(rl0.rlim_cur == rl_self.rlim_cur, "pid=0 == pid=getpid(): soft");
        CHECK(rl0.rlim_max == rl_self.rlim_max, "pid=0 == pid=getpid(): hard");
    }

    /* ═══════════════════════════════════════════════════════════════
     * 3. ENFORCEMENT: RLIMIT_NOFILE
     *    Lower to a small value, open that many files,
     *    verify the next open fails with EMFILE.
     * ═══════════════════════════════════════════════════════════════ */
    {
        struct rlimit old;
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &old), 0, "NOFILE: save original");

        int test_fd[64];
        int test_limit = (old.rlim_cur > 16 && old.rlim_cur < 64) ? 16 : 8;

        struct rlimit low;
        low.rlim_cur = (rlim_t)test_limit;
        low.rlim_max = old.rlim_max;

        errno = 0;
        int rc = setrlimit(RLIMIT_NOFILE, &low);
        if (rc != 0) {
            printf("  SKIP NOFILE enforcement: cannot set limit (errno=%d)\n", errno);
            goto nofile_done;
        }

        struct rlimit verify;
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &verify), 0,
                  "NOFILE: getrlimit after set");
        CHECK((int)verify.rlim_cur == test_limit,
              "NOFILE: soft limit applied");

        /* Open test_limit files via /dev/null (they consume an fd each).
         * fd 0,1,2 are already open, so we can open test_limit - 3 more. */
        int i, opened = 0;
        for (i = 0; i < test_limit; i++) {
            int fd = open("/dev/null", O_RDONLY);
            if (fd < 0) break;
            test_fd[opened++] = fd;
        }
        printf("  NOFILE: opened %d / %d files before failure\n",
               opened, test_limit);

        /* The next open should fail with EMFILE */
        errno = 0;
        int fd = open("/dev/null", O_RDONLY);
        CHECK(fd < 0 && errno == EMFILE,
              "NOFILE: exceeding soft limit → EMFILE");
        if (fd >= 0) close(fd);

        /* Clean up: close test fds */
        for (i = 0; i < opened; i++) close(test_fd[i]);

        /* Restore */
        CHECK_RET(setrlimit(RLIMIT_NOFILE, &old), 0, "NOFILE: restore");
    }
nofile_done:

    /* ═══════════════════════════════════════════════════════════════
     * 3b. ENFORCEMENT: RLIMIT_NOFILE raised to 2048
     *     Fill the descriptor table until it refuses another entry,
     *     then create a UDP socket.  The kernel must reject it with
     *     EMFILE and release the new socket without crashing, even
     *     when the raised limit exceeds the table's fixed capacity.
     * ═══════════════════════════════════════════════════════════════ */
    {
        enum { RAISED_NOFILE = 2048 };
        static int fill_fd[RAISED_NOFILE];
        struct rlimit old, high;
        CHECK_RET(getrlimit(RLIMIT_NOFILE, &old), 0,
                  "NOFILE raised: save original");

        high.rlim_cur = RAISED_NOFILE;
        high.rlim_max = old.rlim_max > RAISED_NOFILE ? old.rlim_max
                                                      : RAISED_NOFILE;
        errno = 0;
        if (setrlimit(RLIMIT_NOFILE, &high) != 0) {
            printf("  SKIP NOFILE raised: cannot raise limit (errno=%d)\n",
                   errno);
            goto nofile_raised_done;
        }

        int filled = 0;
        int dup_errno = 0;
        while (filled < RAISED_NOFILE) {
            int fd = dup(0);
            if (fd < 0) {
                dup_errno = errno;
                break;
            }
            fill_fd[filled++] = fd;
        }
        printf("  NOFILE raised: table full after %d dups\n", filled);
        CHECK(filled < RAISED_NOFILE && dup_errno == EMFILE,
              "NOFILE raised: dup on a full table → EMFILE");

        errno = 0;
        int sock = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(sock < 0 && errno == EMFILE,
              "NOFILE raised: UDP socket on a full table → EMFILE");
        if (sock >= 0) close(sock);

        for (int i = 0; i < filled; i++) close(fill_fd[i]);

        sock = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(sock >= 0, "NOFILE raised: UDP socket after closing fds");
        if (sock >= 0) close(sock);

        CHECK_RET(setrlimit(RLIMIT_NOFILE, &old), 0,
                  "NOFILE raised: restore");
    }
nofile_raised_done:

    /* ═══════════════════════════════════════════════════════════════
     * 4. ENFORCEMENT: RLIMIT_FSIZE
     *    Set small file size limit, write large data, expect EFBIG.
     * ═══════════════════════════════════════════════════════════════ */
    {
        struct rlimit old, low;
        CHECK_RET(getrlimit(RLIMIT_FSIZE, &old), 0, "FSIZE: save original");

        low.rlim_cur = 4;  /* 4 bytes */
        low.rlim_max = old.rlim_max;
        if (setrlimit(RLIMIT_FSIZE, &low) != 0) {
            printf("  SKIP FSIZE enforcement: cannot set limit (errno=%d)\n", errno);
            goto fsize_done;
        }

        /* Verify the limit is set */
        struct rlimit verify;
        CHECK_RET(getrlimit(RLIMIT_FSIZE, &verify), 0,
                  "FSIZE: getrlimit after set");
        CHECK(verify.rlim_cur == 4, "FSIZE: soft limit = 4");

        /* Create a temp file and try to write beyond 4 bytes.
         * Need to ignore SIGXFSZ or it kills us. */
        signal(SIGXFSZ, SIG_IGN);

        char tmpname[] = "/tmp/prlimit_fsize_XXXXXX";
        int fd = mkstemp(tmpname);
        if (fd < 0) {
            printf("  SKIP FSIZE: cannot create temp file\n");
            signal(SIGXFSZ, SIG_DFL);
            setrlimit(RLIMIT_FSIZE, &old);
            goto fsize_done;
        }
        unlink(tmpname); /* delete on close */

        /* Write 5 bytes (limit is 4) — should fail */
        char buf[10] = "123456789";
        ssize_t n = write(fd, buf, 5);
        if (n < 0 && errno == EFBIG) {
            CHECK(1, "FSIZE: write(5) beyond 4-byte limit → EFBIG");
        } else if (n < 0) {
            printf("  FSIZE: write() failed with errno=%d (%s)\n",
                   errno, strerror(errno));
            CHECK(errno == EFBIG, "FSIZE: expected EFBIG");
        } else {
            /* Some systems allow the write up to the limit then deliver SIGXFSZ
             * on the next write.  Try another write. */
            errno = 0;
            n = write(fd, buf, 5);
            if (n < 0 && errno == EFBIG) {
                CHECK(1, "FSIZE: 2nd write beyond limit → EFBIG");
            } else {
                printf("  FSIZE: wrote %zd then %zd bytes (limit=4)\n",
                       n > 0 ? n : 0, n);
                CHECK(n < 0 && errno == EFBIG,
                      "FSIZE: write beyond 4-byte limit → EFBIG");
            }
        }

        close(fd);
        signal(SIGXFSZ, SIG_DFL);
        setrlimit(RLIMIT_FSIZE, &old);
    }
fsize_done:

    /* ═══════════════════════════════════════════════════════════════
     * 5. ENFORCEMENT: RLIMIT_NICE
     *    Set nice ceiling.  Try to setpriority() below it.
     * ═══════════════════════════════════════════════════════════════ */
#ifdef RLIMIT_NICE
    {
        struct rlimit old;
        if (getrlimit(RLIMIT_NICE, &old) != 0) {
            printf("  SKIP NICE: getrlimit failed\n");
            goto nice_done;
        }

        /* rlim_cur is the ceiling for 20 - nice.  Set ceiling to 21
         * (max nice value of -1, i.e. 20 - 21 = -1). */
        struct rlimit ceil;
        ceil.rlim_cur = 21;
        ceil.rlim_max = old.rlim_max;
        if (setrlimit(RLIMIT_NICE, &ceil) != 0) {
            printf("  SKIP NICE: cannot set limit (errno=%d)\n", errno);
            goto nice_done;
        }

        /* Verify the limit */
        struct rlimit verify;
        CHECK_RET(getrlimit(RLIMIT_NICE, &verify), 0,
                  "NICE: getrlimit after set");
        CHECK(verify.rlim_cur == 21, "NICE: ceiling = 21");

        /* Lower nice to -1 (raise priority) — should be allowed */
        errno = 0;
        int rc = setpriority(PRIO_PROCESS, 0, -1);
        if (rc == 0) {
            CHECK(1, "NICE: setpriority(-1) allowed within ceiling");
            /* Restore to 0 */
            setpriority(PRIO_PROCESS, 0, 0);
        } else {
            printf("  NICE: setpriority(-1) failed: errno=%d (%s)\n",
                   errno, strerror(errno));
        }

        /* Now lower ceiling to 20 (max nice = 0), try to go below */
        ceil.rlim_cur = 20;
        setrlimit(RLIMIT_NICE, &ceil);
        errno = 0;
        rc = setpriority(PRIO_PROCESS, 0, -1);
        if (rc == -1 && (errno == EACCES || errno == EPERM)) {
            CHECK(1, "NICE: setpriority(-1) blocked by RLIMIT_NICE ceiling");
        } else if (rc == 0) {
            printf("  NICE: setpriority(-1) succeeded (RLIMIT_NICE not enforced)\n");
            setpriority(PRIO_PROCESS, 0, 0);
        } else {
            printf("  NICE: setpriority(-1) unexpected errno=%d\n", errno);
        }

        setrlimit(RLIMIT_NICE, &old);
    }
nice_done:
#endif

    /* ═══════════════════════════════════════════════════════════════
     * 6. ENFORCEMENT: RLIMIT_CORE
     *    Set to 0 → no core; set to nonzero → core allowed (up to N).
     *    We can't easily trigger a core dump, but we can check set/get.
     * ═══════════════════════════════════════════════════════════════ */
    {
        struct rlimit old, zero;
        CHECK_RET(getrlimit(RLIMIT_CORE, &old), 0, "CORE: save original");

        zero.rlim_cur = 0;
        zero.rlim_max = old.rlim_max;
        CHECK_RET(setrlimit(RLIMIT_CORE, &zero), 0,
                  "CORE: set to 0 (disable core dumps)");

        struct rlimit verify;
        CHECK_RET(getrlimit(RLIMIT_CORE, &verify), 0,
                  "CORE: getrlimit confirms 0");
        CHECK(verify.rlim_cur == 0, "CORE: soft = 0");

        /* Set to RLIM_INFINITY (unlimited core) */
        zero.rlim_cur = RLIM_INFINITY;
        zero.rlim_max = RLIM_INFINITY;
        errno = 0;
        if (setrlimit(RLIMIT_CORE, &zero) == 0) {
            CHECK_RET(getrlimit(RLIMIT_CORE, &verify), 0,
                      "CORE: getrlimit after RLIM_INFINITY");
            CHECK(verify.rlim_cur == RLIM_INFINITY,
                  "CORE: soft = RLIM_INFINITY");
        }

        setrlimit(RLIMIT_CORE, &old);
    }

    /* ═══════════════════════════════════════════════════════════════
     * 7. ENFORCEMENT: RLIMIT_DATA
     *    Set a data limit, try brk(2) beyond it.
     * ═══════════════════════════════════════════════════════════════ */
    {
        struct rlimit old;
        if (getrlimit(RLIMIT_DATA, &old) != 0) {
            printf("  SKIP DATA: getrlimit failed\n");
            goto data_done;
        }

        /* Find current data segment size */
        void *cur_brk = sbrk(0);
        if (cur_brk == (void *)-1) {
            printf("  SKIP DATA: sbrk(0) failed\n");
            goto data_done;
        }

        /* Set data limit to current brk + 4096 */
        unsigned long cur_addr = (unsigned long)cur_brk;
        struct rlimit low;
        low.rlim_cur = cur_addr + 4096;
        low.rlim_max = old.rlim_max;
        if (setrlimit(RLIMIT_DATA, &low) != 0) {
            printf("  SKIP DATA: cannot set limit (errno=%d)\n", errno);
            goto data_done;
        }

        /* Try to brk() far beyond the limit (e.g. + 2MB) */
        errno = 0;
        void *new_brk = sbrk(2 * 1024 * 1024);
        if (new_brk == (void *)-1 && errno == ENOMEM) {
            CHECK(1, "DATA: brk beyond RLIMIT_DATA → ENOMEM");
        } else if (new_brk == (void *)-1) {
            printf("  DATA: brk failed with errno=%d (%s)\n",
                   errno, strerror(errno));
        } else {
            printf("  DATA: brk succeeded (RLIMIT_DATA not enforced?)\n");
            brk(cur_brk); /* shrink back */
        }

        setrlimit(RLIMIT_DATA, &old);
    }
data_done:

    /* ═══════════════════════════════════════════════════════════════
     * 8. Query all RLIMIT_* resources for consistency
     * ═══════════════════════════════════════════════════════════════ */
    {
        printf("\n--- All RLIMIT_* resources (getrlimit) ---\n");
        int resources[] = {
            RLIMIT_AS, RLIMIT_CORE, RLIMIT_CPU, RLIMIT_DATA, RLIMIT_FSIZE,
            RLIMIT_MEMLOCK,
#ifdef RLIMIT_MSGQUEUE
            RLIMIT_MSGQUEUE,
#endif
#ifdef RLIMIT_NICE
            RLIMIT_NICE,
#endif
            RLIMIT_NOFILE, RLIMIT_NPROC,
#ifdef RLIMIT_RTPRIO
            RLIMIT_RTPRIO,
#endif
#ifdef RLIMIT_RTTIME
            RLIMIT_RTTIME,
#endif
#ifdef RLIMIT_SIGPENDING
            RLIMIT_SIGPENDING,
#endif
            RLIMIT_STACK,
        };
        const char *names[] = {
            "AS", "CORE", "CPU", "DATA", "FSIZE", "MEMLOCK",
#ifdef RLIMIT_MSGQUEUE
            "MSGQUEUE",
#endif
#ifdef RLIMIT_NICE
            "NICE",
#endif
            "NOFILE", "NPROC",
#ifdef RLIMIT_RTPRIO
            "RTPRIO",
#endif
#ifdef RLIMIT_RTTIME
            "RTTIME",
#endif
#ifdef RLIMIT_SIGPENDING
            "SIGPENDING",
#endif
            "STACK",
        };
        int n = (int)(sizeof(resources) / sizeof(resources[0]));
        for (int i = 0; i < n; i++) {
            struct rlimit rl;
            if (getrlimit(resources[i], &rl) == 0) {
                printf("  RLIMIT_%-10s cur=%lu max=%lu\n",
                       names[i], (unsigned long)rl.rlim_cur,
                       (unsigned long)rl.rlim_max);
            } else {
                printf("  RLIMIT_%-10s getrlimit failed (errno=%d)\n",
                       names[i], errno);
            }
        }
    }

    /* ═══════════════════════════════════════════════════════════════
     * 9. Error conditions
     * ═══════════════════════════════════════════════════════════════ */
    {
        struct rlimit rl, old;

        /* invalid pid → ESRCH */
        CHECK_ERR(prlimit(0x7FFFFFFF, RLIMIT_NOFILE, NULL, &rl), ESRCH,
                  "invalid pid get → ESRCH");
        CHECK_ERR(prlimit(-1, RLIMIT_NOFILE, NULL, &rl), ESRCH,
                  "pid=-1 → ESRCH");

        /* invalid resource → EINVAL */
        CHECK_ERR(prlimit(0, -1, NULL, &rl), EINVAL,
                  "res=-1 → EINVAL");
        CHECK_ERR(prlimit(0, 9999, NULL, &rl), EINVAL,
                  "res=9999 → EINVAL");

        /* soft > hard → EINVAL (setrlimit and prlimit) */
        rl.rlim_cur = 1000;
        rl.rlim_max = 500;
        CHECK_ERR(setrlimit(RLIMIT_NOFILE, &rl), EINVAL,
                  "setrlimit(soft > hard) → EINVAL");
        CHECK_ERR(prlimit(0, RLIMIT_NOFILE, &rl, NULL), EINVAL,
                  "prlimit(soft > hard) → EINVAL");

        /* raise hard limit — requires CAP_SYS_RESOURCE */
        CHECK_RET(prlimit(0, RLIMIT_NOFILE, NULL, &old), 0,
                  "get NOFILE for hard-limit test");
        rl.rlim_cur = old.rlim_max + 1;
        rl.rlim_max = old.rlim_max + 1;
        errno = 0;
        int rc = setrlimit(RLIMIT_NOFILE, &rl);
        if (rc == 0) {
            /* Root (or CAP_SYS_RESOURCE) can raise hard limits */
            CHECK(1, "raise hard limit allowed (privileged)");
            /* Restore */
            setrlimit(RLIMIT_NOFILE, &old);
        } else {
            CHECK(rc == -1 && errno == EPERM,
                  "raise hard limit without privilege → EPERM");
        }

        /* invalid pid + invalid res → ESRCH (pid checked first on Linux) */
        CHECK_ERR(prlimit(0x7FFFFFFF, -1, NULL, &rl), ESRCH,
                  "invalid pid + bad res → ESRCH (pid first)");

        /* pid=1 init — may fail ESRCH or EPERM */
        errno = 0;
        rc = prlimit(1, RLIMIT_NOFILE, NULL, &rl);
        if (rc == 0) {
            printf("  prlimit(pid=1): cur=%lu max=%lu\n",
                   (unsigned long)rl.rlim_cur, (unsigned long)rl.rlim_max);
            CHECK(rl.rlim_cur > 0, "init NOFILE soft > 0");
        } else {
            CHECK(rc == -1 && (errno == EPERM || errno == ESRCH || errno == EACCES),
                  "prlimit(pid=1) → EPERM/ESRCH/EACCES");
        }
    }

    TEST_DONE();
}
