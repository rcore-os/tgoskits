#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define TEST_CAP_VERSION_3 0x20080522U
#define TEST_CAP_U32S_3 2
#define TEST_CAP_SETUID 7

#ifndef PR_CAP_AMBIENT
#define PR_CAP_AMBIENT 47
#endif

#ifndef PR_CAP_AMBIENT_IS_SET
#define PR_CAP_AMBIENT_IS_SET 1
#endif

#ifndef PR_CAP_AMBIENT_RAISE
#define PR_CAP_AMBIENT_RAISE 2
#endif

struct test_cap_header {
    unsigned int version;
    int pid;
};

struct test_cap_data {
    unsigned int effective;
    unsigned int permitted;
    unsigned int inheritable;
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

static int write_child_file(pid_t pid, const char *name, const char *text)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "/proc/%d/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return write_file(path, text);
}

static int capget_self(struct test_cap_data data[TEST_CAP_U32S_3])
{
    struct test_cap_header header = {
        .version = TEST_CAP_VERSION_3,
        .pid = 0,
    };
    memset(data, 0, sizeof(*data) * TEST_CAP_U32S_3);
    return (int)syscall(SYS_capget, &header, data);
}

static int capset_self(struct test_cap_data data[TEST_CAP_U32S_3])
{
    struct test_cap_header header = {
        .version = TEST_CAP_VERSION_3,
        .pid = 0,
    };
    return (int)syscall(SYS_capset, &header, data);
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

/*
 * A privileged parent writes a non-zero host UID into its user-namespace
 * child's map. StarryOS currently stores the second column directly in the
 * child's credentials and has no namespace-scoped capability checks, so this
 * path must fail closed rather than leave global host privileges behind.
 */
static void nonroot_mapping_drops_host_capabilities(const struct initial_ns *ns)
{
    (void)ns;
    /*
     * The pipes are control channels. Report EPIPE through CHECK and still
     * close both ends and reap the child instead of terminating here.
     * This whole body already runs in its own test child.
     */
    if (signal(SIGPIPE, SIG_IGN) == SIG_ERR) {
        CHECK(0, "ignore SIGPIPE for guarded pipe cleanup");
        return;
    }

    char host_file[128];
    int length = snprintf(host_file, sizeof(host_file),
                          "/tmp/nonzero-uid-map-%d", getpid());
    CHECK(length > 0 && (size_t)length < sizeof(host_file),
          "build a unique root-owned host file path");
    if (length <= 0 || (size_t)length >= sizeof(host_file))
        return;

    int host_fd = open(host_file, O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC,
                       0600);
    CHECK(host_fd >= 0, "create a root-owned 0600 host file");
    if (host_fd < 0)
        return;
    CHECK_RET(write(host_fd, "x", 1), 1, "write the host file");
    CHECK_RET(close(host_fd), 0, "close the host file");

    int ready[2] = { -1, -1 };
    int release[2] = { -1, -1 };
    if (pipe(ready) < 0 || pipe(release) < 0) {
        CHECK(0, "create parent-child synchronization pipes");
        if (ready[0] >= 0)
            close(ready[0]);
        if (ready[1] >= 0)
            close(ready[1]);
        if (release[0] >= 0)
            close(release[0]);
        if (release[1] >= 0)
            close(release[1]);
        unlink(host_file);
        return;
    }

    fflush(stdout);
    pid_t child = fork();
    if (child < 0) {
        CHECK(0, "fork the user-namespace child");
        close(ready[0]);
        close(ready[1]);
        close(release[0]);
        close(release[1]);
        unlink(host_file);
        return;
    }

    if (child == 0) {
        int failed_before = __fail;
        close(ready[0]);
        close(release[1]);
        if (unshare(CLONE_NEWUSER) < 0) {
            CHECK(0, "child creates a user namespace");
            _exit(1);
        }

        struct test_cap_data prefill[TEST_CAP_U32S_3] = { { 0 } };
        prefill[0].effective = 0xffffffffU;
        prefill[0].permitted = 0xffffffffU;
        prefill[0].inheritable = 0xffffffffU;
        prefill[1].effective = 0xffffffffU;
        prefill[1].permitted = 0xffffffffU;
        prefill[1].inheritable = 0xffffffffU;
        CHECK_RET(capset_self(prefill), 0,
                  "prefill capability sets before the uid mapping");

        struct test_cap_data before_map[TEST_CAP_U32S_3] = { { 0 } };
        CHECK_RET(capget_self(before_map), 0,
                  "read capability sets before the uid mapping");
        CHECK(before_map[0].effective != 0 &&
              before_map[0].permitted != 0 &&
              before_map[0].inheritable != 0,
              "capset prefilled permitted, effective and inheritable");

        char token = 'R';
        if (write(ready[1], &token, 1) != 1)
            _exit(1);
        close(ready[1]);
        if (read(release[0], &token, 1) != 1 || token != '1')
            _exit(1);
        close(release[0]);

        CHECK(getuid() == 1000 && geteuid() == 1000,
              "current simplified model stores the non-zero mapped host uid");

        struct test_cap_data after_map[TEST_CAP_U32S_3] = { { 0 } };
        CHECK_RET(capget_self(after_map), 0,
                  "read capability sets after the uid mapping");
        CHECK(after_map[0].effective == 0 && after_map[0].permitted == 0 &&
              after_map[0].inheritable == 0 && after_map[1].effective == 0 &&
              after_map[1].permitted == 0 && after_map[1].inheritable == 0,
              "non-zero mapping clears every process capability set");

        errno = 0;
        int fd = open(host_file, O_RDONLY | O_CLOEXEC);
        CHECK(fd < 0 && errno == EACCES,
              "non-zero mapped child cannot read a root-owned 0600 host file");
        if (fd >= 0)
            close(fd);

        CHECK_ERR(mount("none", "/tmp", "starry-no-such-fs", 0, NULL), EPERM,
                  "non-zero mapped child cannot manage the shared host mount namespace");

        unsigned int ruid = 0;
        unsigned int euid = 0;
        unsigned int suid = 0;
        CHECK_ERR(setresuid(0, 0, 0), EPERM,
                  "non-zero mapped child cannot setresuid to host root");
        CHECK_RET(getresuid(&ruid, &euid, &suid), 0,
                  "read ids after the refused setresuid");
        CHECK(ruid == 1000 && euid == 1000 && suid == 1000,
              "failed setresuid leaves the mapped ids unchanged");
        CHECK_ERR(setuid(0), EPERM,
                  "non-zero mapped child cannot setuid to host root");

        CHECK_RET(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET,
                        TEST_CAP_SETUID, 0, 0), 0,
                  "non-zero mapping leaves CAP_SETUID out of the ambient set");
        CHECK_ERR(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE,
                        TEST_CAP_SETUID, 0, 0), EPERM,
                  "non-zero mapped child cannot raise CAP_SETUID to ambient");
        CHECK_RET(prctl(PR_CAPBSET_READ, TEST_CAP_SETUID), 0,
                  "non-zero mapping removes CAP_SETUID from the bounding set");

        struct test_cap_data inheritable_only[TEST_CAP_U32S_3] = { { 0 } };
        inheritable_only[0].inheritable = 0xffffffffU;
        inheritable_only[1].inheritable = 0xffffffffU;
        CHECK_ERR(capset_self(inheritable_only), EPERM,
                  "non-zero mapping prevents repopulating inheritable capabilities");

        struct test_cap_data restore_all[TEST_CAP_U32S_3] = { { 0 } };
        restore_all[0].effective = 0xffffffffU;
        restore_all[0].permitted = 0xffffffffU;
        restore_all[0].inheritable = 0xffffffffU;
        restore_all[1].effective = 0xffffffffU;
        restore_all[1].permitted = 0xffffffffU;
        restore_all[1].inheritable = 0xffffffffU;
        CHECK_ERR(capset_self(restore_all), EPERM,
                  "non-zero mapping prevents restoring permitted capabilities");

        struct test_cap_data after_refused_capset[TEST_CAP_U32S_3] = { { 0 } };
        CHECK_RET(capget_self(after_refused_capset), 0,
                  "read capability sets after refused capset calls");
        CHECK(after_refused_capset[0].effective == 0 &&
              after_refused_capset[0].permitted == 0 &&
              after_refused_capset[0].inheritable == 0 &&
              after_refused_capset[1].effective == 0 &&
              after_refused_capset[1].permitted == 0 &&
              after_refused_capset[1].inheritable == 0,
              "refused capset leaves every process capability set clear");

        fflush(stdout);
        _exit(__fail > failed_before);
    }

    close(ready[1]);
    close(release[0]);

    char token;
    ssize_t ready_count;
    do {
        ready_count = read(ready[0], &token, 1);
    } while (ready_count < 0 && errno == EINTR);
    close(ready[0]);

    int map_succeeded = 0;
    int release_sent = 0;
    if (ready_count == 1 && token == 'R') {
        int map_result = write_child_file(child, "uid_map", "0 1000 1\n");
        CHECK_RET(map_result, 0,
                  "privileged parent maps child namespace uid 0 to host uid 1000");
        map_succeeded = map_result == 0;
        ssize_t release_result = write(release[1], "1", 1);
        CHECK_RET(release_result, 1, "release the mapped child");
        release_sent = release_result == 1;
    } else {
        CHECK(0, "child reached the user namespace before mapping");
    }
    close(release[1]);

    int status = 0;
    pid_t waited;
    do {
        waited = waitpid(child, &status, 0);
    } while (waited < 0 && errno == EINTR);
    CHECK_RET(waited, child,
              "wait for the user-namespace child");
    CHECK(waited == child && map_succeeded && release_sent &&
          WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "non-zero mapped child preserves the host credential boundary");
    CHECK_RET(unlink(host_file), 0, "remove the root-owned host file");
}

int main(void)
{
    TEST_START("user namespace entry preserves host capability boundaries");
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
    in_child("non-zero uid mapping drops host capabilities", nonroot_mapping_drops_host_capabilities, &ns);
    TEST_DONE();
}
