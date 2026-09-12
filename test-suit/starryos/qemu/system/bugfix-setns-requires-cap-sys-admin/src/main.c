#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <sched.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

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
    TEST_DONE();
}
