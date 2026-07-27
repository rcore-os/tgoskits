#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define CLONE_NEWCGROUP 0x02000000
#define STACK_SIZE (1024 * 1024)

#define GLOBAL_MOUNT "/tmp/cgroup-ns-global"
#define VIEW_MOUNT "/tmp/cgroup-ns-view"
#define LIFETIME_MOUNT "/tmp/cgroup-ns-lifetime-view"
#define PARENT_GROUP GLOBAL_MOUNT "/parent"
#define CHILD_GROUP PARENT_GROUP "/child"
#define SIBLING_GROUP GLOBAL_MOUNT "/sibling"
#define LIFETIME_GROUP GLOBAL_MOUNT "/lifetime"

struct clone_check {
    unsigned long long parent_ns_id;
    const char *expected_path;
};

static void fail(const char *operation)
{
    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", operation, errno,
            strerror(errno));
    exit(1);
}

static void fail_message(const char *message)
{
    fprintf(stderr, "FAIL: %s\n", message);
    exit(1);
}

static void require_true(int condition, const char *message)
{
    if (!condition) {
        fail_message(message);
    }
    printf("PASS: %s\n", message);
}

static void ensure_dir(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        fail(path);
    }
}

static void mount_cgroup2(const char *path)
{
    ensure_dir(path);
    if (mount("none", path, "cgroup2", 0, NULL) != 0) {
        fail("mount cgroup2");
    }
}

static ssize_t read_text_fd(int fd, char *buf, size_t capacity)
{
    if (capacity < 2) {
        errno = EINVAL;
        return -1;
    }

    ssize_t size = read(fd, buf, capacity - 1);
    if (size >= 0) {
        buf[size] = '\0';
    }
    return size;
}

static ssize_t read_text_file(const char *path, char *buf, size_t capacity)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    ssize_t size = read_text_fd(fd, buf, capacity);
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    return size;
}

static void expect_text(const char *path, const char *expected)
{
    char content[PATH_MAX];
    if (read_text_file(path, content, sizeof(content)) < 0) {
        fail(path);
    }
    if (strcmp(content, expected) != 0) {
        fprintf(stderr, "FAIL: %s contains %s, expected %s", path, content,
                expected);
        exit(1);
    }
}

static void expect_proc_path(const char *expected)
{
    expect_text("/proc/self/cgroup", expected);
    printf("PASS: /proc/self/cgroup is %s", expected);
}

static int text_contains_pid(const char *content, pid_t pid)
{
    const char *cursor = content;

    while (*cursor != '\0') {
        char *end = NULL;
        errno = 0;
        long value = strtol(cursor, &end, 10);
        if (cursor != end && errno == 0 && value == (long)pid) {
            return 1;
        }
        while (*cursor != '\0' && *cursor != '\n') {
            cursor++;
        }
        while (*cursor == '\n' || *cursor == '\r') {
            cursor++;
        }
    }
    return 0;
}

static void expect_pid_membership(const char *path, pid_t pid, int present)
{
    char content[4096];
    if (read_text_file(path, content, sizeof(content)) < 0) {
        fail(path);
    }
    require_true(text_contains_pid(content, pid) == present,
                 present ? "cgroup.procs contains expected pid"
                         : "cgroup.procs excludes migrated pid");
}

static void move_pid(const char *cgroup_path, pid_t pid)
{
    char procs_path[PATH_MAX];
    char pid_text[32];

    if (snprintf(procs_path, sizeof(procs_path), "%s/cgroup.procs",
                 cgroup_path) >= (int)sizeof(procs_path)) {
        fail_message("cgroup.procs path overflow");
    }
    if (snprintf(pid_text, sizeof(pid_text), "%d\n", pid) >=
        (int)sizeof(pid_text)) {
        fail_message("pid text overflow");
    }

    int fd = open(procs_path, O_WRONLY);
    if (fd < 0) {
        fail(procs_path);
    }
    ssize_t written = write(fd, pid_text, strlen(pid_text));
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    if (written != (ssize_t)strlen(pid_text)) {
        fail("write cgroup.procs");
    }
}

static unsigned long long get_ns_id(const char *path)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        fail(path);
    }
    struct stat st;
    if (fstat(fd, &st) != 0) {
        close(fd);
        fail(path);
    }
    close(fd);
    return (unsigned long long)st.st_ino;
}

static void wait_child_ok(pid_t pid)
{
    int status = 0;
    if (waitpid(pid, &status, 0) != pid) {
        fail("waitpid");
    }
    require_true(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                 "child exited successfully");
}

static int clone_child(void *argument)
{
    const struct clone_check *check = argument;
    unsigned long long child_ns_id = get_ns_id("/proc/self/ns/cgroup");

    if (child_ns_id == check->parent_ns_id) {
        fprintf(stderr, "FAIL: clone child shares parent cgroup namespace\n");
        _exit(1);
    }
    expect_proc_path(check->expected_path);
    _exit(0);
}

static void test_fork_and_clone(unsigned long long namespace_id)
{
    pid_t pid = fork();
    if (pid < 0) {
        fail("fork");
    }
    if (pid == 0) {
        if (get_ns_id("/proc/self/ns/cgroup") != namespace_id) {
            fprintf(stderr, "FAIL: normal fork changed cgroup namespace\n");
            _exit(1);
        }
        expect_proc_path("0::/\n");
        _exit(0);
    }
    wait_child_ok(pid);

    char *stack = malloc(STACK_SIZE);
    if (stack == NULL) {
        fail("malloc clone stack");
    }
    struct clone_check check = {
        .parent_ns_id = namespace_id,
        .expected_path = "0::/\n",
    };
    pid = clone(clone_child, stack + STACK_SIZE, CLONE_NEWCGROUP | SIGCHLD,
                &check);
    if (pid < 0) {
        free(stack);
        fail("clone(CLONE_NEWCGROUP)");
    }
    wait_child_ok(pid);
    free(stack);
}

static void test_descendant_view(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        fail("fork descendant");
    }
    if (pid == 0) {
        move_pid(CHILD_GROUP, getpid());
        expect_proc_path("0::/child\n");
        _exit(0);
    }
    wait_child_ok(pid);
}

static void test_mount_view(void)
{
    mount_cgroup2(VIEW_MOUNT);

    struct stat root;
    struct stat parent;
    require_true(stat(VIEW_MOUNT "/child", &root) == 0,
                 "namespace-rooted mount exposes descendants");
    require_true(stat(VIEW_MOUNT "/sibling", &root) == -1 && errno == ENOENT,
                 "namespace-rooted mount hides global siblings");
    if (stat(VIEW_MOUNT, &root) != 0 ||
        stat(VIEW_MOUNT "/..", &parent) != 0) {
        fail("stat cgroup mount root");
    }
    require_true(root.st_ino == parent.st_ino,
                 "dotdot at cgroup namespace root cannot escape");
}

static void test_setns_preserves_membership(int parent_nsfd)
{
    move_pid(CHILD_GROUP, getpid());
    expect_proc_path("0::/child\n");

    int procfd = open("/proc/self/cgroup", O_RDONLY);
    if (procfd < 0) {
        fail("open /proc/self/cgroup");
    }

    if (unshare(CLONE_NEWCGROUP) != 0) {
        fail("unshare child-root cgroup namespace");
    }
    expect_proc_path("0::/\n");

    if (setns(parent_nsfd, CLONE_NEWCGROUP) != 0) {
        fail("setns parent cgroup namespace");
    }
    expect_proc_path("0::/child\n");
    expect_pid_membership(CHILD_GROUP "/cgroup.procs", getpid(), 1);

    char content[PATH_MAX];
    if (read_text_fd(procfd, content, sizeof(content)) < 0) {
        close(procfd);
        fail("read opened /proc/self/cgroup");
    }
    close(procfd);
    require_true(strcmp(content, "0::/child\n") == 0,
                 "proc cgroup read uses namespace active at read time");
}

static void test_namespace_and_mount_lifetime(int restore_nsfd)
{
    int ready_pipe[2];
    int exit_pipe[2];
    if (pipe(ready_pipe) != 0 || pipe(exit_pipe) != 0) {
        fail("pipe");
    }

    ensure_dir(LIFETIME_MOUNT);
    pid_t pid = fork();
    if (pid < 0) {
        fail("fork lifetime");
    }
    if (pid == 0) {
        close(ready_pipe[0]);
        close(exit_pipe[1]);
        move_pid(LIFETIME_GROUP, getpid());
        if (unshare(CLONE_NEWCGROUP) != 0) {
            fail("unshare lifetime cgroup namespace");
        }
        mount_cgroup2(LIFETIME_MOUNT);
        expect_proc_path("0::/\n");
        if (write(ready_pipe[1], "R", 1) != 1) {
            fail("signal lifetime ready");
        }
        char release;
        if (read(exit_pipe[0], &release, 1) != 1) {
            fail("wait lifetime release");
        }
        _exit(0);
    }

    close(ready_pipe[1]);
    close(exit_pipe[0]);
    char ready;
    if (read(ready_pipe[0], &ready, 1) != 1) {
        fail("read lifetime ready");
    }
    close(ready_pipe[0]);

    char namespace_path[64];
    if (snprintf(namespace_path, sizeof(namespace_path),
                 "/proc/%d/ns/cgroup", pid) >= (int)sizeof(namespace_path)) {
        fail_message("namespace path overflow");
    }
    int lifetime_nsfd = open(namespace_path, O_RDONLY);
    if (lifetime_nsfd < 0) {
        fail("open child cgroup namespace");
    }

    if (write(exit_pipe[1], "X", 1) != 1) {
        fail("release lifetime child");
    }
    close(exit_pipe[1]);
    wait_child_ok(pid);

    expect_pid_membership(LIFETIME_MOUNT "/cgroup.procs", pid, 0);
    if (setns(lifetime_nsfd, CLONE_NEWCGROUP) != 0) {
        fail("setns exited child cgroup namespace");
    }
    expect_proc_path("0::/../parent/child\n");
    if (setns(restore_nsfd, CLONE_NEWCGROUP) != 0) {
        fail("restore parent cgroup namespace");
    }

    errno = 0;
    require_true(rmdir(LIFETIME_GROUP) == -1 && errno == EBUSY,
                 "namespace fd and mount pin cgroup root");
    close(lifetime_nsfd);
    errno = 0;
    require_true(rmdir(LIFETIME_GROUP) == -1 && errno == EBUSY,
                 "live mount keeps cgroup root pinned");
    if (umount2(LIFETIME_MOUNT, 0) != 0) {
        fail("unmount lifetime cgroup2");
    }
    if (rmdir(LIFETIME_GROUP) != 0) {
        fail("remove released lifetime cgroup");
    }
    printf("PASS: final close and unmount release cgroup root\n");
}

int main(void)
{
    mount_cgroup2(GLOBAL_MOUNT);
    ensure_dir(PARENT_GROUP);
    ensure_dir(CHILD_GROUP);
    ensure_dir(SIBLING_GROUP);
    ensure_dir(LIFETIME_GROUP);

    pid_t self = getpid();
    move_pid(PARENT_GROUP, self);
    expect_pid_membership(PARENT_GROUP "/cgroup.procs", self, 1);
    expect_pid_membership(GLOBAL_MOUNT "/cgroup.procs", self, 0);

    unsigned long long root_namespace_id =
        get_ns_id("/proc/self/ns/cgroup");
    if (unshare(CLONE_NEWCGROUP) != 0) {
        fail("unshare(CLONE_NEWCGROUP)");
    }
    unsigned long long parent_namespace_id =
        get_ns_id("/proc/self/ns/cgroup");
    require_true(parent_namespace_id != root_namespace_id,
                 "unshare creates a distinct cgroup namespace");
    expect_proc_path("0::/\n");

    int parent_nsfd = open("/proc/self/ns/cgroup", O_RDONLY);
    if (parent_nsfd < 0) {
        fail("open parent cgroup namespace");
    }

    test_fork_and_clone(parent_namespace_id);
    test_descendant_view();
    test_mount_view();
    test_setns_preserves_membership(parent_nsfd);
    test_namespace_and_mount_lifetime(parent_nsfd);

    close(parent_nsfd);
    printf("TEST_CGROUP_NS_PASSED\n");
    return 0;
}
