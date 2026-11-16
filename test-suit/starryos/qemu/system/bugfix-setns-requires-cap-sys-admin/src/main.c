#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <sched.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_clone3
#define SYS_clone3 435
#endif

enum {
    USER_NAMESPACE_LEVELS = 33,
    DEPTH_CLONE_STACK_SIZE = 64 * 1024,
};

struct clone3_args {
    unsigned long long flags;
    unsigned long long pidfd;
    unsigned long long child_tid;
    unsigned long long parent_tid;
    unsigned long long exit_signal;
    unsigned long long stack;
    unsigned long long stack_size;
    unsigned long long tls;
    unsigned long long set_tid;
    unsigned long long set_tid_size;
    unsigned long long cgroup;
};

struct initial_ns {
    int uts;
    int ipc;
    int net;
    int pidfd;
};

static int write_file(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    ssize_t n = write(fd, text, strlen(text));
    int saved = errno;
    close(fd);
    errno = saved;
    return n == (ssize_t)strlen(text) ? 0 : -1;
}

static int configure_self_user_namespace(void)
{
    if (write_file("/proc/self/uid_map", "0 0 1") != 0)
        return 1;
    if (write_file("/proc/self/setgroups", "deny") != 0)
        return 2;
    return write_file("/proc/self/gid_map", "0 0 1") != 0 ? 3 : 0;
}

static int user_namespace_inode(unsigned long long *inode)
{
    struct stat status;
    int fd = open("/proc/self/ns/user", O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    int result = fstat(fd, &status);
    int saved = errno;
    close(fd);
    errno = saved;
    if (result != 0)
        return -1;
    *inode = (unsigned long long)status.st_ino;
    return 0;
}

static int depth_clone_child(void *argument)
{
    (void)argument;
    _exit(0);
}

static int prepare_low_nofile_hard_limit(void)
{
    const rlim_t safe_hard_limit = 256;
    struct rlimit current;
    struct rlimit lowered;

    errno = 0;
    if (syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, NULL, &current) != 0)
        return 1;
    lowered.rlim_max =
        current.rlim_max > safe_hard_limit ? safe_hard_limit : current.rlim_max;
    lowered.rlim_cur =
        current.rlim_cur > lowered.rlim_max ? lowered.rlim_max : current.rlim_cur;
    errno = 0;
    if (syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, &lowered, NULL) != 0)
        return 2;
    return 0;
}

static void expect_deep_hard_limit_raise_denied(const char *context)
{
    struct rlimit before;
    struct rlimit requested;
    struct rlimit old = {
        .rlim_cur = 0x13579bdf,
        .rlim_max = 0x2468ace0,
    };
    struct rlimit after;
    char message[160];

    errno = 0;
    if (syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, NULL, &before) != 0) {
        snprintf(message, sizeof(message),
                 "%s: query the lowered RLIMIT_NOFILE", context);
        CHECK(0, message);
        return;
    }
    if (before.rlim_max == RLIM_INFINITY) {
        snprintf(message, sizeof(message),
                 "%s: hard limit remains finite and safely below nr_open",
                 context);
        CHECK(0, message);
        return;
    }

    requested.rlim_cur = before.rlim_cur;
    requested.rlim_max = before.rlim_max + 1;
    errno = 0;
    long result =
        syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, &requested, &old);
    int raise_errno = errno;

    memset(&after, 0, sizeof(after));
    errno = 0;
    int after_result =
        syscall(SYS_prlimit64, 0, RLIMIT_NOFILE, NULL, &after);
    int after_errno = errno;

    snprintf(message, sizeof(message),
             "%s: hard-limit raise is denied without initial-namespace capability",
             context);
    CHECK(result == -1 && raise_errno == EPERM &&
              old.rlim_cur == 0x13579bdf &&
              old.rlim_max == 0x2468ace0,
          message);

    snprintf(message, sizeof(message),
             "%s: denied hard-limit raise leaves RLIMIT_NOFILE unchanged",
             context);
    CHECK(after_result == 0 && after.rlim_cur == before.rlim_cur &&
              after.rlim_max == before.rlim_max,
          message);
    if (after_result != 0)
        errno = after_errno;
}

static void user_namespace_depth_limit(const struct initial_ns *unused)
{
    _Alignas(16) static unsigned char clone_stack[DEPTH_CLONE_STACK_SIZE];
    unsigned long long namespace_before = 0;
    unsigned long long namespace_after = 0;
    char message[128];

    (void)unused;
    CHECK(geteuid() == 0,
          "user-namespace depth fixture starts in the initial namespace");

    for (int level = 1; level <= USER_NAMESPACE_LEVELS; level++) {
        errno = 0;
        int result = unshare(CLONE_NEWUSER);
        snprintf(message, sizeof(message),
                 "create user namespace level %d", level);
        CHECK(result == 0, message);
        if (result != 0)
            return;

        int mapping_result = configure_self_user_namespace();
        snprintf(message, sizeof(message),
                 "map uid/gid 0 to 0 at user namespace level %d", level);
        CHECK(mapping_result == 0, message);
        if (mapping_result != 0)
            return;
    }

    CHECK(user_namespace_inode(&namespace_before) == 0,
          "capture the level-33 user namespace identity");
    CHECK(prepare_low_nofile_hard_limit() == 0,
          "lower RLIMIT_NOFILE hard limit before deep namespace probes");

    /*
     * This probe intentionally does not return early: a flat-capability
     * implementation must still reach and fail the independent clone,
     * clone3, unshare, and namespace-identity assertions below.
     */
    expect_deep_hard_limit_raise_denied("before depth-limit failures");

    errno = 0;
    pid_t child = clone(depth_clone_child,
                        clone_stack + sizeof(clone_stack),
                        CLONE_NEWUSER | SIGCHLD, NULL);
    int clone_errno = errno;
    CHECK(child == -1 && clone_errno == ENOSPC,
          "legacy clone(CLONE_NEWUSER) at level 33 returns ENOSPC");
    if (child > 0) {
        int status = 0;
        while (waitpid(child, &status, 0) == -1 && errno == EINTR) {
        }
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "reap child unexpectedly created by legacy clone");
    }
    CHECK(user_namespace_inode(&namespace_after) == 0 &&
              namespace_after == namespace_before,
          "legacy clone failure leaves the current user namespace unchanged");

    struct clone3_args args = {
        .flags = CLONE_NEWUSER,
        .exit_signal = SIGCHLD,
    };
    errno = 0;
    long clone3_result = syscall(SYS_clone3, &args, sizeof(args));
    int clone3_errno = errno;
    if (clone3_result == 0)
        _exit(0);
    CHECK(clone3_result == -1 && clone3_errno == ENOSPC,
          "raw clone3(CLONE_NEWUSER) at level 33 returns ENOSPC");
    if (clone3_result > 0) {
        int status = 0;
        while (waitpid((pid_t)clone3_result, &status, 0) == -1 &&
               errno == EINTR) {
        }
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "reap child unexpectedly created by raw clone3");
    }
    CHECK(user_namespace_inode(&namespace_after) == 0 &&
              namespace_after == namespace_before,
          "raw clone3 failure leaves the current user namespace unchanged");

    errno = 0;
    int unshare_result = unshare(CLONE_NEWUSER);
    int unshare_errno = errno;
    CHECK(unshare_result == -1 && unshare_errno == ENOSPC,
          "unshare(CLONE_NEWUSER) at level 33 returns ENOSPC");
    CHECK(user_namespace_inode(&namespace_after) == 0 &&
              namespace_after == namespace_before,
          "failed depth-limited unshare leaves the current user namespace unchanged");

    expect_deep_hard_limit_raise_denied("after depth-limit failures");
}

/* Runs `body` in a child so dropped privileges and new namespaces stay contained. */
static void in_child(const char *name, void (*body)(const struct initial_ns *), const struct initial_ns *ns)
{
    fflush(stdout);
    int failed_before = __fail;
    pid_t child = fork();
    if (child == 0) {
        body(ns);
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, name);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, name);
}

static void drop_to_nobody(void)
{
    CHECK(setgid(65534) == 0 && setuid(65534) == 0, "drop to uid/gid 65534");
    /* setuid() leaves the task non-dumpable, which would make its /proc maps root-owned. */
    CHECK_RET(prctl(PR_SET_DUMPABLE, 1), 0, "keep /proc/self maps writable");
}

static void refuse_initial_namespaces(const struct initial_ns *ns)
{
    CHECK_ERR(setns(ns->uts, CLONE_NEWUTS), EPERM, "setns(initial uts nsfd) refused");
    CHECK_ERR(setns(ns->ipc, CLONE_NEWIPC), EPERM, "setns(initial ipc nsfd) refused");
    CHECK_ERR(setns(ns->net, CLONE_NEWNET), EPERM, "setns(initial net nsfd) refused");
    CHECK_ERR(setns(ns->pidfd, CLONE_NEWUTS), EPERM, "setns(pidfd, CLONE_NEWUTS) refused");
    CHECK_ERR(setns(ns->pidfd, CLONE_NEWIPC), EPERM, "setns(pidfd, CLONE_NEWIPC) refused");
    CHECK_ERR(setns(ns->pidfd, CLONE_NEWNET), EPERM, "setns(pidfd, CLONE_NEWNET) refused");
}

/* Root in the initial user namespace holds CAP_SYS_ADMIN over every namespace that user namespace owns. */
static void root_in_initial_user_ns(const struct initial_ns *ns)
{
    CHECK_RET(setns(ns->uts, CLONE_NEWUTS), 0, "root joins the initial uts namespace by nsfd");
    CHECK_RET(setns(ns->pidfd, CLONE_NEWUTS | CLONE_NEWIPC), 0, "root joins the initial uts and ipc namespaces by pidfd");
    CHECK_RET(unshare(CLONE_NEWUTS), 0, "root creates a uts namespace");
    int created = open("/proc/self/ns/uts", O_RDONLY | O_CLOEXEC);
    CHECK(created >= 0, "open the created uts namespace");
    CHECK_RET(setns(ns->pidfd, CLONE_NEWUTS), 0, "root returns to the initial uts namespace by pidfd");
    CHECK_RET(setns(created, CLONE_NEWUTS), 0, "root rejoins the uts namespace it created by nsfd");
}

/* Without CAP_SYS_ADMIN, joining any namespace is refused. */
static void unprivileged_task(const struct initial_ns *ns)
{
    drop_to_nobody();
    refuse_initial_namespaces(ns);
}

/* An unprivileged writer may only map its own ids; mapping id 0 would grant root. */
static void rootless_writer_cannot_map_root(const struct initial_ns *ns)
{
    drop_to_nobody();
    CHECK_RET(unshare(CLONE_NEWUSER), 0, "create a child user namespace");
    CHECK_ERR(write_file("/proc/self/uid_map", "0 0 1"), EPERM, "uid_map mapping outside uid 0 refused");
    CHECK_RET(write_file("/proc/self/setgroups", "deny"), 0, "deny setgroups before writing gid_map");
    CHECK_ERR(write_file("/proc/self/gid_map", "0 0 1"), EPERM, "gid_map mapping outside gid 0 refused");
    CHECK(getuid() != 0 && getgid() != 0, "still not root inside the child user namespace");
    refuse_initial_namespaces(ns);
}

/* Capabilities obtained inside a child user namespace do not reach the initial namespaces. */
static void rootless_mapped_to_own_uid(const struct initial_ns *ns)
{
    drop_to_nobody();
    CHECK_RET(unshare(CLONE_NEWUSER), 0, "create a child user namespace");
    CHECK_RET(write_file("/proc/self/uid_map", "0 65534 1"), 0, "uid_map mapping its own uid accepted");
    refuse_initial_namespaces(ns);
}

/* Even root loses its hold on the initial namespaces once it enters a child user namespace. */
static void root_after_entering_child_user_ns(const struct initial_ns *ns)
{
    CHECK_RET(unshare(CLONE_NEWUSER), 0, "root creates a child user namespace");
    CHECK_RET(write_file("/proc/self/uid_map", "0 0 1"), 0, "root maps its own uid 0");
    refuse_initial_namespaces(ns);
}

int main(void)
{
    TEST_START("setns requires CAP_SYS_ADMIN over the target namespace");
    CHECK(getuid() == 0, "runs as root so it can drop privileges");
    struct initial_ns ns = {
        .uts = open("/proc/self/ns/uts", O_RDONLY | O_CLOEXEC),
        .ipc = open("/proc/self/ns/ipc", O_RDONLY | O_CLOEXEC),
        .net = open("/proc/self/ns/net", O_RDONLY | O_CLOEXEC),
        .pidfd = (int)syscall(SYS_pidfd_open, getpid(), 0),
    };
    CHECK(ns.uts >= 0 && ns.ipc >= 0 && ns.net >= 0 && ns.pidfd >= 0, "open initial namespace fds and a pidfd");

    in_child("root in the initial user namespace", root_in_initial_user_ns, &ns);
    in_child("unprivileged task", unprivileged_task, &ns);
    in_child("rootless writer cannot map root", rootless_writer_cannot_map_root, &ns);
    in_child("rootless task mapped to its own uid", rootless_mapped_to_own_uid, &ns);
    in_child("root after entering a child user namespace", root_after_entering_child_user_ns, &ns);
    in_child("user namespace parent-chain depth limit", user_namespace_depth_limit, &ns);
    TEST_DONE();
}
