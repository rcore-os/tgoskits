#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PR_SET_KEEPCAPS
#define PR_SET_KEEPCAPS 8
#endif

#ifndef PR_GET_KEEPCAPS
#define PR_GET_KEEPCAPS 7
#endif

#define LINUX_CAPABILITY_VERSION_3 0x20080522U

struct capability_header {
    uint32_t version;
    int32_t pid;
};

struct capability_data {
    uint32_t effective;
    uint32_t permitted;
    uint32_t inheritable;
};

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

static long prctl_raw(int option, unsigned long arg2)
{
    return syscall(SYS_prctl, option, arg2, 0, 0, 0);
}

static int read_capabilities(struct capability_data data[2])
{
    struct capability_header header = {
        .version = LINUX_CAPABILITY_VERSION_3,
        .pid = 0,
    };
    memset(data, 0, sizeof(*data) * 2);
    return (int)syscall(SYS_capget, &header, data);
}

static uint64_t capability_mask(const struct capability_data data[2],
                                int permitted)
{
    uint32_t low = permitted ? data[0].permitted : data[0].effective;
    uint32_t high = permitted ? data[1].permitted : data[1].effective;
    return (uint64_t)low | ((uint64_t)high << 32);
}

static void expect_value(const char *name, long expected)
{
    errno = 0;
    long ret = prctl_raw(PR_GET_KEEPCAPS, 0);
    if (ret == expected) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "ret=%ld errno=%d (%s), expected %ld",
             ret, errno, strerror(errno), expected);
    note_fail(name, detail);
}

static void expect_set(const char *name, unsigned long value)
{
    errno = 0;
    long ret = prctl_raw(PR_SET_KEEPCAPS, value);
    if (ret == 0) {
        note_pass(name);
        return;
    }

    char detail[160];
    snprintf(detail, sizeof(detail), "ret=%ld errno=%d (%s), expected 0",
             ret, errno, strerror(errno));
    note_fail(name, detail);
}

static void expect_invalid_set(unsigned long value, const char *name)
{
    errno = 0;
    long ret = prctl_raw(PR_SET_KEEPCAPS, value);
    int saved_errno = errno;
    if (ret == -1 && saved_errno == EINVAL) {
        note_pass(name);
        return;
    }

    char detail[192];
    snprintf(detail, sizeof(detail),
             "ret=%ld errno=%d (%s), expected -1/EINVAL for state=%lu",
             ret, saved_errno, strerror(saved_errno), value);
    note_fail(name, detail);
}

static int verify_setuid_transition(void)
{
    if (geteuid() != 0) {
        fprintf(stderr, "setuid capability transition requires euid 0\n");
        return 1;
    }

    struct capability_data before[2];
    struct capability_data after[2];
    if (read_capabilities(before) != 0) {
        perror("capget before setuid");
        return 1;
    }
    uint64_t permitted_before = capability_mask(before, 1);
    if (permitted_before == 0) {
        fprintf(stderr, "root process unexpectedly has no permitted capabilities\n");
        return 1;
    }

    if (prctl_raw(PR_SET_KEEPCAPS, 1) != 0) {
        perror("PR_SET_KEEPCAPS before setuid");
        return 1;
    }
    if (setuid(65534) != 0) {
        perror("setuid(65534)");
        return 1;
    }
    if (read_capabilities(after) != 0) {
        perror("capget after setuid");
        return 1;
    }

    uint64_t permitted_after = capability_mask(after, 1);
    uint64_t effective_after = capability_mask(after, 0);
    if (permitted_after != permitted_before) {
        fprintf(stderr,
                "permitted capabilities changed: before=%#llx after=%#llx\n",
                (unsigned long long)permitted_before,
                (unsigned long long)permitted_after);
        return 1;
    }
    if (effective_after != 0) {
        fprintf(stderr, "effective capabilities not cleared: %#llx\n",
                (unsigned long long)effective_after);
        return 1;
    }
    if (prctl_raw(PR_GET_KEEPCAPS, 0) != 1) {
        fprintf(stderr, "keepcaps flag was not retained across setuid\n");
        return 1;
    }
    return 0;
}

static int verify_exec_reset(const char *self_path)
{
    if (prctl_raw(PR_SET_KEEPCAPS, 1) != 0) {
        perror("PR_SET_KEEPCAPS before exec");
        return 1;
    }
    execl(self_path, self_path, "--verify-exec-reset", NULL);
    perror("execl self");
    return 1;
}

static void expect_child_success(const char *name,
                                 int (*child_fn)(const char *),
                                 const char *argument)
{
    fflush(NULL);
    pid_t pid = fork();
    if (pid < 0) {
        note_fail(name, "fork failed");
        return;
    }
    if (pid == 0) {
        _exit(child_fn(argument) == 0 ? 0 : 1);
    }

    int status;
    if (waitpid(pid, &status, 0) == pid && WIFEXITED(status) &&
        WEXITSTATUS(status) == 0) {
        note_pass(name);
        return;
    }

    char detail[96];
    snprintf(detail, sizeof(detail), "child status=%#x", status);
    note_fail(name, detail);
}

static int setuid_child_adapter(const char *unused)
{
    (void)unused;
    return verify_setuid_transition();
}

struct sibling_keepcaps_state {
    atomic_int ready;
    atomic_int start;
    long keepcaps;
    int keepcaps_errno;
    long setuid_result;
    int setuid_errno;
    int capget_before_result;
    int capget_before_errno;
    int capget_after_result;
    int capget_after_errno;
    uint64_t permitted_before;
    uint64_t permitted_after;
};

static void *sibling_keepcaps_worker(void *argument)
{
    struct sibling_keepcaps_state *state = argument;
    atomic_store_explicit(&state->ready, 1, memory_order_release);
    while (atomic_load_explicit(&state->start, memory_order_acquire) == 0) {
        sched_yield();
    }

    errno = 0;
    state->keepcaps = prctl_raw(PR_GET_KEEPCAPS, 0);
    state->keepcaps_errno = errno;

    struct capability_data before[2];
    errno = 0;
    state->capget_before_result = read_capabilities(before);
    state->capget_before_errno = errno;
    if (state->capget_before_result == 0) {
        state->permitted_before = capability_mask(before, 1);
    }

    errno = 0;
    state->setuid_result = syscall(SYS_setuid, 65534);
    state->setuid_errno = errno;

    struct capability_data after[2];
    errno = 0;
    state->capget_after_result = read_capabilities(after);
    state->capget_after_errno = errno;
    if (state->capget_after_result == 0) {
        state->permitted_after = capability_mask(after, 1);
    }
    return NULL;
}

static int verify_sibling_thread_isolation(const char *unused)
{
    (void)unused;
    if (geteuid() != 0) {
        fprintf(stderr, "thread-local keepcaps test requires euid 0\n");
        return 1;
    }

    struct capability_data controlling_before[2];
    if (read_capabilities(controlling_before) != 0) {
        perror("controlling thread capget before sibling setuid");
        return 1;
    }
    uint64_t controlling_permitted_before =
        capability_mask(controlling_before, 1);
    if (controlling_permitted_before == 0) {
        fprintf(stderr,
                "controlling thread unexpectedly has no permitted capabilities\n");
        return 1;
    }

    struct sibling_keepcaps_state state = {0};
    atomic_init(&state.ready, 0);
    atomic_init(&state.start, 0);

    pthread_t sibling;
    int create_result =
        pthread_create(&sibling, NULL, sibling_keepcaps_worker, &state);
    if (create_result != 0) {
        fprintf(stderr, "pthread_create failed: %s\n", strerror(create_result));
        return 1;
    }
    while (atomic_load_explicit(&state.ready, memory_order_acquire) == 0) {
        sched_yield();
    }

    if (prctl_raw(PR_SET_KEEPCAPS, 1) != 0) {
        perror("PR_SET_KEEPCAPS in controlling thread");
        atomic_store_explicit(&state.start, 1, memory_order_release);
        pthread_join(sibling, NULL);
        return 1;
    }
    atomic_store_explicit(&state.start, 1, memory_order_release);

    int join_result = pthread_join(sibling, NULL);
    if (join_result != 0) {
        fprintf(stderr, "pthread_join failed: %s\n", strerror(join_result));
        return 1;
    }
    if (state.keepcaps != 0) {
        fprintf(stderr,
                "sibling observed keepcaps=%ld errno=%d (%s), expected 0\n",
                state.keepcaps, state.keepcaps_errno,
                strerror(state.keepcaps_errno));
        return 1;
    }
    if (state.capget_before_result != 0 || state.permitted_before == 0) {
        fprintf(stderr,
                "sibling capget before setuid failed: ret=%d errno=%d (%s) "
                "permitted=%#llx\n",
                state.capget_before_result, state.capget_before_errno,
                strerror(state.capget_before_errno),
                (unsigned long long)state.permitted_before);
        return 1;
    }
    if (state.setuid_result != 0) {
        fprintf(stderr, "sibling setuid failed: ret=%ld errno=%d (%s)\n",
                state.setuid_result, state.setuid_errno,
                strerror(state.setuid_errno));
        return 1;
    }
    if (state.capget_after_result != 0 || state.permitted_after != 0) {
        fprintf(stderr,
                "sibling retained permitted capabilities after setuid: "
                "ret=%d errno=%d (%s) permitted=%#llx\n",
                state.capget_after_result, state.capget_after_errno,
                strerror(state.capget_after_errno),
                (unsigned long long)state.permitted_after);
        return 1;
    }

    errno = 0;
    long controlling_keepcaps = prctl_raw(PR_GET_KEEPCAPS, 0);
    if (controlling_keepcaps != 1) {
        fprintf(stderr,
                "sibling setuid changed controlling keepcaps=%ld errno=%d "
                "(%s), expected 1\n",
                controlling_keepcaps, errno, strerror(errno));
        return 1;
    }

    struct capability_data controlling_after[2];
    if (read_capabilities(controlling_after) != 0) {
        perror("controlling thread capget after sibling setuid");
        return 1;
    }
    uint64_t controlling_permitted_after =
        capability_mask(controlling_after, 1);
    uint64_t controlling_effective_after =
        capability_mask(controlling_after, 0);
    if (controlling_permitted_after != controlling_permitted_before) {
        fprintf(stderr,
                "sibling setuid changed controlling permitted capabilities: "
                "before=%#llx after=%#llx\n",
                (unsigned long long)controlling_permitted_before,
                (unsigned long long)controlling_permitted_after);
        return 1;
    }
    if (controlling_effective_after != 0) {
        fprintf(stderr,
                "sibling setuid did not clear controlling effective "
                "capabilities: %#llx\n",
                (unsigned long long)controlling_effective_after);
        return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--verify-exec-reset") == 0) {
        return prctl_raw(PR_GET_KEEPCAPS, 0) == 0 ? 0 : 1;
    }

    printf("=== bug-prctl-keepcaps ===\n");

    expect_value("PR_GET_KEEPCAPS defaults to 0 after exec", 0);
    expect_set("PR_SET_KEEPCAPS enables the flag", 1);
    expect_value("PR_GET_KEEPCAPS observes the enabled flag", 1);
    expect_invalid_set(2, "PR_SET_KEEPCAPS rejects state 2");
    expect_invalid_set(ULONG_MAX,
                       "PR_SET_KEEPCAPS rejects an unsigned high value");
    expect_value("invalid states leave the flag unchanged", 1);
    expect_set("PR_SET_KEEPCAPS clears the flag", 0);
    expect_value("PR_GET_KEEPCAPS observes the cleared flag", 0);

    expect_child_success(
        "keepcaps preserves permitted and clears effective caps across setuid",
        setuid_child_adapter, NULL);
    expect_child_success(
        "PR_SET_KEEPCAPS stays local to the calling thread",
        verify_sibling_thread_isolation, NULL);
    expect_child_success("exec resets the keepcaps flag", verify_exec_reset,
                         argv[0]);

    printf("=== Results: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("ALL TESTS PASSED\n");
        return 0;
    }
    printf("SOME TESTS FAILED\n");
    return 1;
}
