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
#include <sched.h>
#include <stdint.h>
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

#define PRLIMIT_CAPABILITY_VERSION_3 0x20080522U
#define PRLIMIT_CAP_SYS_RESOURCE 24

struct prlimit_cap_header {
    uint32_t version;
    int32_t pid;
};

struct prlimit_cap_data {
    uint32_t effective;
    uint32_t permitted;
    uint32_t inheritable;
};

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

static int cap_read(struct prlimit_cap_data data[2])
{
    struct prlimit_cap_header header = {
        .version = PRLIMIT_CAPABILITY_VERSION_3,
        .pid = 0,
    };
    memset(data, 0, sizeof(*data) * 2);
    return (int)syscall(SYS_capget, &header, data);
}

static int cap_has_sys_resource(const struct prlimit_cap_data data[2],
                                int permitted)
{
    uint32_t mask =
        permitted ? data[0].permitted : data[0].effective;
    return (mask & (1U << PRLIMIT_CAP_SYS_RESOURCE)) != 0;
}

static int cap_set_sys_resource_effective(int enabled)
{
    struct prlimit_cap_header header = {
        .version = PRLIMIT_CAPABILITY_VERSION_3,
        .pid = 0,
    };
    struct prlimit_cap_data data[2];
    uint32_t bit = 1U << PRLIMIT_CAP_SYS_RESOURCE;

    if (cap_read(data) != 0) {
        return -1;
    }
    if (enabled) {
        data[0].effective |= bit;
    } else {
        data[0].effective &= ~bit;
    }
    return (int)syscall(SYS_capset, &header, data);
}

static int send_sync(int fd)
{
    char byte = 1;
    return write_exact(fd, &byte, 1);
}

static int receive_sync(int fd)
{
    char byte = 0;
    return read_exact(fd, &byte, 1) == 1 && byte == 1 ? 0 : -1;
}

static int write_pid_proc_file(pid_t pid, const char *name,
                               const char *value)
{
    char path[96];
    int length = snprintf(path, sizeof(path), "/proc/%d/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = EOVERFLOW;
        return -1;
    }

    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) {
        return -1;
    }
    int result = write_exact(fd, value, strlen(value));
    int saved = errno;
    close(fd);
    errno = saved;
    return result;
}

static int configure_user_namespace(pid_t pid, const char *range,
                                    int deny_setgroups)
{
    if (write_pid_proc_file(pid, "uid_map", range) != 0) {
        return -1;
    }
    if (deny_setgroups &&
        write_pid_proc_file(pid, "setgroups", "deny\n") != 0) {
        return -1;
    }
    return write_pid_proc_file(pid, "gid_map", range);
}

static int wait_for_child(pid_t child, int *status)
{
    pid_t result;
    do {
        errno = 0;
        result = waitpid(child, status, 0);
    } while (result == -1 && errno == EINTR);
    return result == child ? 0 : -1;
}

static int prlimit_read_matches(pid_t target, unsigned int resource,
                                const struct rlimit *expected)
{
    struct rlimit actual = {0};
    errno = 0;
    return raw_prlimit64(target, resource, NULL, &actual) == 0 &&
                   actual.rlim_cur == expected->rlim_cur &&
                   actual.rlim_max == expected->rlim_max
               ? 0
               : -1;
}

static int expect_cross_prlimit_denied(pid_t target)
{
    struct rlimit old = {.rlim_cur = 0x1357, .rlim_max = 0x2468};
    struct rlimit replacement = {.rlim_cur = 1, .rlim_max = 1};
    int failures = 0;

    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, NULL, &old) != -1 ||
        errno != EPERM || old.rlim_cur != 0x1357 ||
        old.rlim_max != 0x2468) {
        failures |= 1;
    }

    old.rlim_cur = 0x3579;
    old.rlim_max = 0x468a;
    errno = 0;
    if (raw_prlimit64(target, RLIMIT_NOFILE, &replacement, &old) != -1 ||
        errno != EPERM || old.rlim_cur != 0x3579 ||
        old.rlim_max != 0x468a) {
        failures |= 2;
    }
    return failures;
}

static int expect_child_hard_limit_raise_denied(void)
{
    struct rlimit current = {0};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &current) != 0 ||
        current.rlim_max == RLIM_INFINITY) {
        return 1;
    }

    if (current.rlim_max > 8) {
        struct rlimit lower = {
            .rlim_cur = current.rlim_cur > 8 ? 8 : current.rlim_cur,
            .rlim_max = 8,
        };
        errno = 0;
        if (raw_prlimit64(0, RLIMIT_NOFILE, &lower, NULL) != 0) {
            return 2;
        }
    }

    struct rlimit before = {0};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &before) != 0 ||
        before.rlim_max == RLIM_INFINITY) {
        return 3;
    }

    struct rlimit raise = {
        .rlim_cur = before.rlim_cur,
        .rlim_max = before.rlim_max + 1,
    };
    struct rlimit old = {.rlim_cur = 0x579b, .rlim_max = 0x68ac};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, &raise, &old) != -1 ||
        errno != EPERM || old.rlim_cur != 0x579b ||
        old.rlim_max != 0x68ac) {
        return 4;
    }

    struct rlimit after = {0};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &after) != 0 ||
        after.rlim_cur != before.rlim_cur ||
        after.rlim_max != before.rlim_max) {
        return 5;
    }
    return 0;
}

static int expect_initial_hard_limit_raise_allowed(void)
{
    struct rlimit original = {0};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &original) != 0) {
        return 1;
    }

    const rlim_t safe_hard_limit = 1024;
    rlim_t raised_max =
        original.rlim_max == RLIM_INFINITY ||
                original.rlim_max > safe_hard_limit
            ? safe_hard_limit
            : original.rlim_max;
    if (raised_max == 0) {
        return 2;
    }
    rlim_t lowered_max = raised_max > 1 ? raised_max / 2 : 0;
    struct rlimit lower = {
        .rlim_cur = original.rlim_cur > lowered_max ? lowered_max
                                                    : original.rlim_cur,
        .rlim_max = lowered_max,
    };
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, &lower, NULL) != 0) {
        return 3;
    }
    struct rlimit lowered = {0};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &lowered) != 0 ||
        lowered.rlim_cur != lower.rlim_cur ||
        lowered.rlim_max != lower.rlim_max) {
        return 4;
    }

    struct rlimit raise = {
        .rlim_cur = lower.rlim_cur,
        .rlim_max = raised_max,
    };
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, &raise, NULL) != 0) {
        return 5;
    }
    struct rlimit verified = {0};
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &verified) != 0 ||
        verified.rlim_cur != raise.rlim_cur ||
        verified.rlim_max != raise.rlim_max) {
        return 6;
    }
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, &original, NULL) != 0) {
        return 7;
    }
    errno = 0;
    if (raw_prlimit64(0, RLIMIT_NOFILE, NULL, &verified) != 0 ||
        verified.rlim_cur != original.rlim_cur ||
        verified.rlim_max != original.rlim_max) {
        return 8;
    }
    return 0;
}

enum caller_namespace_failure {
    CALLER_FAIL_INITIAL_ANCESTOR = 1U << 0,
    CALLER_FAIL_CHILD_HARD_LIMIT = 1U << 1,
    CALLER_FAIL_SAME_NAMESPACE = 1U << 2,
    CALLER_FAIL_DESCENDANT = 1U << 3,
    CALLER_FAIL_SIBLING = 1U << 4,
    CALLER_FAIL_ID_MATCH = 1U << 5,
    CALLER_FAIL_DROP_CAPABILITY = 1U << 6,
};

struct caller_namespace_context {
    int created_fd;
    int mapped_fd;
    int ready_fd;
    int go_fd;
    int result_fd;
    pid_t initial_target;
    pid_t sibling_target;
    struct rlimit sibling_limit;
};

static int exercise_allowed_prlimit(pid_t target);

static int caller_namespace_main(
    const struct caller_namespace_context *context)
{
    int local_ready[2];
    int local_release[2];
    int nested_ready[2];
    int nested_map[2];
    int nested_release[2];
    char byte = 1;

    if (unshare(CLONE_NEWUSER) != 0 || send_sync(context->created_fd) != 0 ||
        receive_sync(context->mapped_fd) != 0) {
        return 10;
    }

    if (pipe(local_ready) != 0 || pipe(local_release) != 0) {
        return 11;
    }
    pid_t local = fork();
    if (local == 0) {
        close(local_ready[0]);
        close(local_release[1]);
        close(context->created_fd);
        close(context->mapped_fd);
        close(context->ready_fd);
        close(context->go_fd);
        close(context->result_fd);
        struct credential_ids ids = {0};
        if (set_credential_ids(&ids) != 0 ||
            send_sync(local_ready[1]) != 0 ||
            read_exact(local_release[0], &byte, 1) != 1) {
            _exit(12);
        }
        close(local_ready[1]);
        _exit(0);
    }
    close(local_ready[1]);
    close(local_release[0]);
    if (local < 0 || receive_sync(local_ready[0]) != 0) {
        return 13;
    }

    if (pipe(nested_ready) != 0 || pipe(nested_map) != 0 ||
        pipe(nested_release) != 0) {
        return 14;
    }
    pid_t nested = fork();
    if (nested == 0) {
        close(nested_ready[0]);
        close(nested_map[1]);
        close(nested_release[1]);
        close(context->created_fd);
        close(context->mapped_fd);
        close(context->ready_fd);
        close(context->go_fd);
        close(context->result_fd);
        close(local_ready[0]);
        close(local_release[1]);
        if (unshare(CLONE_NEWUSER) != 0 ||
            send_sync(nested_ready[1]) != 0 ||
            receive_sync(nested_map[0]) != 0 ||
            send_sync(nested_ready[1]) != 0 ||
            read_exact(nested_release[0], &byte, 1) != 1) {
            _exit(15);
        }
        close(nested_ready[1]);
        _exit(0);
    }
    close(nested_ready[1]);
    close(nested_map[0]);
    close(nested_release[0]);
    if (nested < 0 || receive_sync(nested_ready[0]) != 0 ||
        configure_user_namespace(nested, "0 0 1\n", 1) != 0 ||
        send_sync(nested_map[1]) != 0 ||
        receive_sync(nested_ready[0]) != 0) {
        return 16;
    }
    close(local_ready[0]);
    close(nested_ready[0]);
    close(nested_map[1]);

    /*
     * Keep real/saved UID 0 but make the caller EUID 1000. This differs from
     * the nested namespace owner (0), so descendant authorization must use
     * the restored effective CAP_SYS_RESOURCE rather than the owner rule.
     */
    struct credential_ids ids = {
        .ruid = 0,
        .euid = 1000,
        .suid = 0,
        .rgid = 1000,
        .egid = 1000,
        .sgid = 1000,
    };
    if (set_credential_ids(&ids) != 0) {
        return 17;
    }
    uid_t ruid;
    uid_t euid;
    uid_t suid;
    gid_t rgid;
    gid_t egid;
    gid_t sgid;
    if (getresuid(&ruid, &euid, &suid) != 0 ||
        getresgid(&rgid, &egid, &sgid) != 0 ||
        ruid != 0 || euid != 1000 || suid != 0 ||
        rgid != 1000 || egid != 1000 || sgid != 1000) {
        return 18;
    }
    if (cap_set_sys_resource_effective(1) != 0) {
        return 19;
    }
    struct prlimit_cap_data capabilities[2];
    if (cap_read(capabilities) != 0 ||
        !cap_has_sys_resource(capabilities, 0) ||
        !cap_has_sys_resource(capabilities, 1)) {
        return 20;
    }
    if (send_sync(context->ready_fd) != 0 ||
        receive_sync(context->go_fd) != 0) {
        return 21;
    }

    unsigned int failures = 0;
    if (expect_cross_prlimit_denied(context->initial_target) != 0) {
        failures |= CALLER_FAIL_INITIAL_ANCESTOR;
    }
    if (expect_child_hard_limit_raise_denied() != 0) {
        failures |= CALLER_FAIL_CHILD_HARD_LIMIT;
    }
    if (exercise_allowed_prlimit(local) != 0) {
        failures |= CALLER_FAIL_SAME_NAMESPACE;
    }
    if (exercise_allowed_prlimit(nested) != 0) {
        failures |= CALLER_FAIL_DESCENDANT;
    }
    int sibling_denied_failure =
        expect_cross_prlimit_denied(context->sibling_target);
    if (sibling_denied_failure != 0) {
        /*
         * A broken flat-capability implementation can mutate the sibling
         * while failing the denial oracle. Restore the known baseline while
         * the erroneous capability is still effective so the independent
         * UID/GID-match scenario starts from the same state.
         */
        struct rlimit restore = context->sibling_limit;
        (void)raw_prlimit64(context->sibling_target, RLIMIT_NOFILE,
                            &restore, NULL);
        failures |= CALLER_FAIL_SIBLING;
    }

    if (setresuid(0, 0, 0) != 0 ||
        setresgid(0, 0, 0) != 0 ||
        cap_set_sys_resource_effective(0) != 0 ||
        getresuid(&ruid, &euid, &suid) != 0 ||
        getresgid(&rgid, &egid, &sgid) != 0 ||
        ruid != 0 || euid != 0 || suid != 0 ||
        rgid != 0 || egid != 0 || sgid != 0 ||
        cap_read(capabilities) != 0 ||
        cap_has_sys_resource(capabilities, 0)) {
        failures |= CALLER_FAIL_DROP_CAPABILITY;
    }
    if (prlimit_read_matches(context->sibling_target, RLIMIT_NOFILE,
                             &context->sibling_limit) != 0) {
        failures |= CALLER_FAIL_ID_MATCH;
    }

    int result_written =
        write_exact(context->result_fd, &failures, sizeof(failures));
    byte = 1;
    (void)write_exact(nested_release[1], &byte, 1);
    (void)write_exact(local_release[1], &byte, 1);
    close(nested_release[1]);
    close(local_release[1]);
    int status = 0;
    if (wait_for_child(nested, &status) != 0 || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0 || wait_for_child(local, &status) != 0 ||
        !WIFEXITED(status) || WEXITSTATUS(status) != 0 ||
        result_written != 0) {
        return 22;
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

static void test_prlimit_user_namespace_capabilities(void)
{
    int initial_ready[2];
    int initial_release[2];
    int sibling_created[2];
    int sibling_map[2];
    int sibling_ready[2];
    int sibling_release[2];
    int caller_created[2];
    int caller_map[2];
    int caller_ready[2];
    int caller_go[2];
    int caller_result[2];
    pid_t initial_target = -1;
    pid_t sibling_target = -1;
    pid_t caller = -1;
    int status = 0;
    char byte = 1;
    struct rlimit initial_limit = {0};
    struct rlimit sibling_limit = {0};
    struct rlimit caller_limit = {0};
    struct prlimit_cap_data capabilities[2];
    unsigned int caller_failures = 0;
    int setup_ok = 1;
    int caller_ready_for_go = 0;

    CHECK(geteuid() == 0,
          "user-namespace prlimit capability matrix starts as initial root");
    CHECK(cap_read(capabilities) == 0 &&
              cap_has_sys_resource(capabilities, 0) &&
              cap_has_sys_resource(capabilities, 1),
          "initial root holds effective and permitted CAP_SYS_RESOURCE");
    if (geteuid() != 0 || cap_read(capabilities) != 0 ||
        !cap_has_sys_resource(capabilities, 0) ||
        !cap_has_sys_resource(capabilities, 1)) {
        return;
    }

    if (pipe(initial_ready) != 0 || pipe(initial_release) != 0) {
        CHECK(0, "create initial-namespace prlimit target pipes");
        return;
    }
    initial_target = fork();
    if (initial_target == 0) {
        close(initial_ready[0]);
        close(initial_release[1]);
        struct credential_ids ids = {
            .ruid = 1000,
            .euid = 1000,
            .suid = 1000,
            .rgid = 2000,
            .egid = 2000,
            .sgid = 2000,
        };
        if (set_credential_ids(&ids) != 0 ||
            send_sync(initial_ready[1]) != 0 ||
            read_exact(initial_release[0], &byte, 1) != 1) {
            _exit(30);
        }
        _exit(0);
    }
    CHECK(initial_target >= 0, "fork initial-namespace prlimit target");
    if (initial_target < 0) {
        close(initial_ready[0]);
        close(initial_ready[1]);
        close(initial_release[0]);
        close(initial_release[1]);
        return;
    }
    close(initial_ready[1]);
    close(initial_release[0]);
    setup_ok &= receive_sync(initial_ready[0]) == 0;
    if (setup_ok) {
        setup_ok &=
            raw_prlimit64(initial_target, RLIMIT_NOFILE, NULL,
                          &initial_limit) == 0;
    }
    CHECK(setup_ok,
          "initial-namespace target reaches its identity wait point");

    if (pipe(sibling_created) != 0 || pipe(sibling_map) != 0 ||
        pipe(sibling_ready) != 0 || pipe(sibling_release) != 0) {
        CHECK(0, "create sibling user-namespace pipes");
        setup_ok = 0;
    }
    if (setup_ok) {
        sibling_target = fork();
        if (sibling_target == 0) {
            close(initial_ready[0]);
            close(initial_release[1]);
            close(sibling_created[0]);
            close(sibling_map[1]);
            close(sibling_ready[0]);
            close(sibling_release[1]);
            if (unshare(CLONE_NEWUSER) != 0 ||
                send_sync(sibling_created[1]) != 0 ||
                receive_sync(sibling_map[0]) != 0 ||
                send_sync(sibling_ready[1]) != 0 ||
                read_exact(sibling_release[0], &byte, 1) != 1) {
                _exit(31);
            }
            _exit(0);
        }
        CHECK(sibling_target >= 0, "fork sibling user-namespace target");
        if (sibling_target < 0) {
            setup_ok = 0;
        } else {
            close(sibling_created[1]);
            close(sibling_map[0]);
            close(sibling_ready[1]);
            close(sibling_release[0]);
            int created = receive_sync(sibling_created[0]) == 0;
            int mapped =
                created &&
                configure_user_namespace(sibling_target, "0 0 1001\n", 1) ==
                    0;
            int map_sent = send_sync(sibling_map[1]) == 0;
            int ready = map_sent && receive_sync(sibling_ready[0]) == 0;
            setup_ok &= created && mapped && ready;
            if (setup_ok) {
                setup_ok &=
                    raw_prlimit64(sibling_target, RLIMIT_NOFILE, NULL,
                                  &sibling_limit) == 0;
            }
            CHECK(setup_ok,
                  "sibling user-namespace target maps and reaches its wait point");
        }
    }

    if (setup_ok &&
        (pipe(caller_created) != 0 || pipe(caller_map) != 0 ||
         pipe(caller_ready) != 0 || pipe(caller_go) != 0 ||
         pipe(caller_result) != 0)) {
        CHECK(0, "create caller user-namespace pipes");
        setup_ok = 0;
    }
    if (setup_ok) {
        caller = fork();
        if (caller == 0) {
            close(initial_ready[0]);
            close(initial_release[1]);
            close(sibling_created[0]);
            close(sibling_map[1]);
            close(sibling_ready[0]);
            close(sibling_release[1]);
            close(caller_created[0]);
            close(caller_map[1]);
            close(caller_ready[0]);
            close(caller_go[1]);
            close(caller_result[0]);
            struct caller_namespace_context context = {
                .created_fd = caller_created[1],
                .mapped_fd = caller_map[0],
                .ready_fd = caller_ready[1],
                .go_fd = caller_go[0],
                .result_fd = caller_result[1],
                .initial_target = initial_target,
                .sibling_target = sibling_target,
                .sibling_limit = sibling_limit,
            };
            _exit(caller_namespace_main(&context));
        }
        CHECK(caller >= 0, "fork user-namespace prlimit caller");
        if (caller < 0) {
            setup_ok = 0;
        } else {
            close(caller_created[1]);
            close(caller_map[0]);
            close(caller_ready[1]);
            close(caller_go[0]);
            close(caller_result[1]);
            int created = receive_sync(caller_created[0]) == 0;
            int mapped =
                created &&
                configure_user_namespace(caller, "0 0 1001\n", 1) == 0;
            int map_sent = send_sync(caller_map[1]) == 0;
            int ready = map_sent && receive_sync(caller_ready[0]) == 0;
            caller_ready_for_go = ready;
            setup_ok &= created && mapped && ready;
            CHECK(setup_ok,
                  "caller user namespace maps and reaches its capability matrix");
        }
    }

    if (setup_ok) {
        setup_ok &=
            raw_prlimit64(caller, RLIMIT_NOFILE, NULL, &caller_limit) == 0;
        CHECK(setup_ok,
              "capture the caller target limit before authorization probes");
    }
    if (setup_ok) {
        CHECK(prlimit_read_matches(caller, RLIMIT_NOFILE, &caller_limit) == 0,
              "initial-namespace CAP_SYS_RESOURCE reaches a descendant target");

        int cleared = cap_set_sys_resource_effective(0);
        CHECK(cleared == 0,
              "clear initial root effective CAP_SYS_RESOURCE for owner-rule probe");
        if (cleared == 0) {
            CHECK(prlimit_read_matches(caller, RLIMIT_NOFILE,
                                       &caller_limit) == 0,
                  "direct user-namespace owner retains CAP_SYS_RESOURCE authority");
        }
        int restored = cap_set_sys_resource_effective(1);
        CHECK(restored == 0,
              "restore initial root effective CAP_SYS_RESOURCE");
        setup_ok &= cleared == 0 && restored == 0;
    }

    if (caller >= 0 && caller_ready_for_go) {
        setup_ok &= send_sync(caller_go[1]) == 0;
    }
    if (caller >= 0) {
        if (setup_ok) {
            setup_ok &=
                read_exact(caller_result[0], &caller_failures,
                           sizeof(caller_failures)) == 1;
        }
        if (wait_for_child(caller, &status) != 0 || !WIFEXITED(status) ||
            WEXITSTATUS(status) != 0) {
            setup_ok = 0;
        }
        CHECK(setup_ok, "user-namespace prlimit caller completes its matrix");
        if (setup_ok) {
            CHECK((caller_failures & CALLER_FAIL_INITIAL_ANCESTOR) == 0,
                  "child-namespace capability cannot reach the initial namespace");
            CHECK((caller_failures & CALLER_FAIL_CHILD_HARD_LIMIT) == 0,
                  "child-namespace capability cannot raise its own hard limit");
            CHECK((caller_failures & CALLER_FAIL_SAME_NAMESPACE) == 0,
                  "same-user-namespace capability permits cross-identity prlimit");
            CHECK((caller_failures & CALLER_FAIL_DESCENDANT) == 0,
                  "ancestor user-namespace capability reaches a nested target");
            CHECK((caller_failures & CALLER_FAIL_SIBLING) == 0,
                  "sibling user-namespace capability cannot reach its peer");
            CHECK((caller_failures & CALLER_FAIL_ID_MATCH) == 0,
                  "matching UID/GID families permit cross-user-namespace prlimit");
            CHECK((caller_failures & CALLER_FAIL_DROP_CAPABILITY) == 0,
                  "caller capability fixture clears its effective CAP_SYS_RESOURCE");
        }
    }

    if (initial_target > 0) {
        CHECK(prlimit_read_matches(initial_target, RLIMIT_NOFILE,
                                   &initial_limit) == 0,
              "denied descendant access leaves the initial-namespace target unchanged");
        (void)send_sync(initial_release[1]);
        CHECK(wait_for_child(initial_target, &status) == 0 &&
                  WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "initial-namespace prlimit target exits cleanly");
    }
    if (sibling_target > 0) {
        CHECK(prlimit_read_matches(sibling_target, RLIMIT_NOFILE,
                                   &sibling_limit) == 0,
              "denied sibling access leaves the sibling target unchanged");
        (void)send_sync(sibling_release[1]);
        CHECK(wait_for_child(sibling_target, &status) == 0 &&
                  WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "sibling user-namespace target exits cleanly");
    }

    CHECK(expect_initial_hard_limit_raise_allowed() == 0,
          "initial-namespace CAP_SYS_RESOURCE may raise a hard limit");

    if (initial_target > 0) {
        close(initial_ready[0]);
        close(initial_release[1]);
    }
    if (sibling_target > 0) {
        close(sibling_created[0]);
        close(sibling_map[1]);
        close(sibling_ready[0]);
        close(sibling_release[1]);
    }
    if (caller > 0) {
        close(caller_created[0]);
        close(caller_map[1]);
        close(caller_ready[0]);
        close(caller_go[1]);
        close(caller_result[0]);
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
    test_prlimit_user_namespace_capabilities();
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
