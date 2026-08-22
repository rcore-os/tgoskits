#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int pass_count;
static int fail_count;

#define CHECK(cond, msg) do {                                             \
    if (cond) {                                                           \
        printf("  PASS | %s\n", msg);                                   \
        pass_count++;                                                    \
    } else {                                                             \
        printf("  FAIL | %s | errno=%d (%s)\n",                         \
               msg, errno, strerror(errno));                             \
        fail_count++;                                                    \
    }                                                                    \
} while (0)

#define CGROUP2_PATH "/tmp/cg-pids"
#define PRE_ENABLE_PATH CGROUP2_PATH "/before-enable"
#define CHILD_PATH CGROUP2_PATH "/limited"
#define CLONE_INTO_PATH CGROUP2_PATH "/clone-into"
#define NESTED_PARENT_PATH CGROUP2_PATH "/nested-parent"
#define NESTED_CHILD_PATH NESTED_PARENT_PATH "/nested-child"
#define CLONE_STACK_SIZE (64 * 1024)
#define CLONE_INTO_CGROUP (1ULL << 33)

struct clone_args {
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

static int clone_child(void *arg)
{
    (void)arg;
    return 0;
}

static ssize_t read_text(const char *path, char *buf, size_t capacity)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t result = read(fd, buf, capacity - 1);
    int saved_errno = errno;
    if (result >= 0) {
        buf[result] = '\0';
    }
    close(fd);
    errno = saved_errno;
    return result;
}

static int write_text(const char *path, const char *value)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        return -1;
    }
    ssize_t expected = (ssize_t)strlen(value);
    ssize_t result = write(fd, value, (size_t)expected);
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    return result == expected ? 0 : -1;
}

static long read_number(const char *path)
{
    char buf[128];
    ssize_t nread = read_text(path, buf, sizeof(buf));
    if (nread < 0) {
        return -1;
    }
    errno = 0;
    char *end = NULL;
    long value = strtol(buf, &end, 10);
    if (end == buf || errno != 0) {
        errno = EINVAL;
        return -1;
    }
    return value;
}

static void expect_text(const char *path, const char *expected, const char *msg)
{
    char buf[128];
    ssize_t nread = read_text(path, buf, sizeof(buf));
    CHECK(nread >= 0 && strcmp(buf, expected) == 0, msg);
}

static void expect_missing(const char *path, const char *msg)
{
    errno = 0;
    int result = access(path, F_OK);
    int saved_errno = errno;
    errno = saved_errno;
    CHECK(result == -1 && saved_errno == ENOENT, msg);
}

static void expect_write_permission_denied(const char *path, const char *value,
                                           const char *msg)
{
    errno = 0;
    int fd = open(path, O_WRONLY);
    int saved_errno = errno;
    ssize_t result = -1;
    if (fd >= 0) {
        errno = 0;
        result = write(fd, value, strlen(value));
        saved_errno = errno;
        close(fd);
    }
    errno = saved_errno;
    CHECK(result == -1 && (saved_errno == EPERM || saved_errno == EACCES), msg);
}

static void expect_write_errno(const char *path, const char *value,
                               int expected_errno, const char *msg)
{
    errno = 0;
    int fd = open(path, O_WRONLY);
    int saved_errno = errno;
    ssize_t result = -1;
    if (fd >= 0) {
        errno = 0;
        result = write(fd, value, strlen(value));
        saved_errno = errno;
        close(fd);
    }
    errno = saved_errno;
    CHECK(result == -1 && saved_errno == expected_errno, msg);
}

int main(void)
{
    printf("================================================\n");
    printf("  TEST: cgroup-pids\n");
    printf("================================================\n");

    CHECK(mkdir(CGROUP2_PATH, 0755) == 0 || errno == EEXIST,
          "create cgroup2 mountpoint");
    errno = 0;
    int mount_result = mount("none", CGROUP2_PATH, "cgroup2", 0, NULL);
    CHECK(mount_result == 0, "mount cgroup2");
    if (mount_result != 0) {
        return 1;
    }

    char controllers[128];
    ssize_t nread = read_text(CGROUP2_PATH "/cgroup.controllers",
                               controllers, sizeof(controllers));
    CHECK(nread >= 0 && strstr(controllers, "pids") != NULL,
          "root advertises pids");
    CHECK(mkdir(PRE_ENABLE_PATH, 0755) == 0 || errno == EEXIST,
          "create child before enabling pids");
    expect_missing(PRE_ENABLE_PATH "/pids.max",
                   "pids.max is absent before parent enables pids");
    expect_missing(CGROUP2_PATH "/pids.max",
                   "root does not expose pids interface files");
    expect_missing(CGROUP2_PATH "/pids.peak",
                   "root does not expose pids.peak");
    CHECK(write_text(CGROUP2_PATH "/cgroup.subtree_control", "+pids") == 0,
          "enable pids for child cgroups");
    CHECK(access(PRE_ENABLE_PATH "/pids.max", F_OK) == 0,
          "existing child gains pids.max after enable");
    CHECK(access(PRE_ENABLE_PATH "/pids.current", F_OK) == 0,
          "existing child gains pids.current after enable");
    CHECK(access(PRE_ENABLE_PATH "/pids.peak", F_OK) == 0,
          "existing child gains pids.peak after enable");
    CHECK(access(PRE_ENABLE_PATH "/pids.events", F_OK) == 0,
          "existing child gains pids.events after enable");
    CHECK(mkdir(CHILD_PATH, 0755) == 0 || errno == EEXIST,
          "create limited child cgroup");
    CHECK(access(CHILD_PATH "/pids.max", F_OK) == 0,
          "child exposes pids.max");
    CHECK(access(CHILD_PATH "/pids.current", F_OK) == 0,
          "child exposes pids.current");
    CHECK(access(CHILD_PATH "/pids.peak", F_OK) == 0,
          "child exposes pids.peak");
    CHECK(access(CHILD_PATH "/pids.events", F_OK) == 0,
          "child exposes pids.events");
    expect_text(CHILD_PATH "/pids.max", "max\n",
                "pids.max starts unlimited");
    expect_write_errno(CHILD_PATH "/pids.max", "-1", EINVAL,
                       "pids.max rejects negative limits");
    expect_text(CHILD_PATH "/pids.max", "max\n",
                "invalid pids.max write leaves limit unchanged");
    expect_text(CHILD_PATH "/pids.events", "max 0\n",
                "pids.events starts at zero");
    expect_text(CHILD_PATH "/pids.peak", "0\n",
                "pids.peak starts at zero");
    expect_write_permission_denied(CHILD_PATH "/pids.current", "0",
                                   "pids.current is read-only");
    expect_write_permission_denied(CHILD_PATH "/pids.peak", "0",
                                   "pids.peak is read-only");
    expect_write_permission_denied(CHILD_PATH "/pids.events", "max 0",
                                   "pids.events is read-only");

    CHECK(write_text(CHILD_PATH "/cgroup.procs", "0") == 0,
          "migrate current process into child");
    CHECK(read_number(CHILD_PATH "/pids.current") == 1,
          "migration charges the current task");
    CHECK(read_number(CHILD_PATH "/pids.peak") == 1,
          "migration raises pids.peak");
    CHECK(write_text(CHILD_PATH "/pids.max", "2") == 0,
          "set pids.max to two tasks");
    expect_text(CHILD_PATH "/pids.max", "2\n",
                "pids.max reads back the configured value");

    int gate[2];
    CHECK(pipe(gate) == 0, "create child synchronization pipe");
    if (fail_count != 0) {
        return 1;
    }

    pid_t first = fork();
    if (first == 0) {
        close(gate[1]);
        char token;
        (void)read(gate[0], &token, sizeof(token));
        close(gate[0]);
        _exit(0);
    }
    CHECK(first > 0, "first fork stays within pids.max");
    if (first < 0) {
        close(gate[0]);
        close(gate[1]);
        return 1;
    }
    close(gate[0]);

    errno = 0;
    pid_t second = fork();
    int second_errno = errno;
    CHECK(second == -1 && second_errno == EAGAIN,
          "second fork is rejected with EAGAIN");
    if (second > 0) {
        (void)waitpid(second, NULL, 0);
    }
    CHECK(read_number(CHILD_PATH "/pids.current") == 2,
          "pids.current includes parent and held child");
    CHECK(read_number(CHILD_PATH "/pids.peak") == 2,
          "held child raises pids.peak");

    void *clone_stack = malloc(CLONE_STACK_SIZE);
    CHECK(clone_stack != NULL, "allocate clone stack");
    if (clone_stack != NULL) {
        pid_t parent_tid = -1;
        errno = 0;
        int clone_result = clone(clone_child,
                                 (char *)clone_stack + CLONE_STACK_SIZE,
                                 CLONE_PARENT_SETTID | SIGCHLD, NULL,
                                 &parent_tid);
        int clone_errno = errno;
        CHECK(clone_result == -1 && clone_errno == EAGAIN,
              "clone with parent TID is rejected with EAGAIN");
        CHECK(parent_tid == -1,
              "denied clone leaves the parent TID pointer unchanged");
        if (clone_result > 0) {
            (void)waitpid(clone_result, NULL, 0);
        }
        free(clone_stack);
    }

    char event_text[128];
    nread = read_text(CHILD_PATH "/pids.events", event_text, sizeof(event_text));
    CHECK(nread >= 0 && strstr(event_text, "max 2") != NULL,
          "pids.events records both denied task creations");

    char release = 'x';
    CHECK(write(gate[1], &release, sizeof(release)) == (ssize_t)sizeof(release),
          "release held child");
    close(gate[1]);
    CHECK(waitpid(first, NULL, 0) == first, "child exit is observed");
    CHECK(read_number(CHILD_PATH "/pids.current") == 1,
          "child exit releases one pids charge");
    CHECK(read_number(CHILD_PATH "/pids.peak") == 2,
          "child exit does not lower pids.peak");
    CHECK(write_text(CGROUP2_PATH "/cgroup.procs", "0") == 0,
          "echo 0 returns current process to root");
    CHECK(read_number(CHILD_PATH "/pids.current") == 0,
          "migration back to root releases child charge");
    CHECK(read_number(CHILD_PATH "/pids.peak") == 2,
          "migration back does not lower pids.peak");

    CHECK(mkdir(CLONE_INTO_PATH, 0755) == 0 || errno == EEXIST,
          "create clone3 target cgroup");
    CHECK(write_text(CLONE_INTO_PATH "/pids.max", "0") == 0,
          "set clone3 target pids.max to zero");
    int clone_into_fd = open(CLONE_INTO_PATH, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    CHECK(clone_into_fd >= 0, "open clone3 target cgroup");
    if (clone_into_fd >= 0) {
        pid_t clone_into_parent_tid = -1;
        struct clone_args args = {
            .flags = CLONE_INTO_CGROUP | CLONE_PARENT_SETTID,
            .parent_tid = (unsigned long long)&clone_into_parent_tid,
            .exit_signal = SIGCHLD,
            .cgroup = (unsigned long long)clone_into_fd,
        };
        errno = 0;
        pid_t clone_into = (pid_t)syscall(SYS_clone3, &args, sizeof(args));
        int clone_into_errno = errno;
        if (clone_into == 0) {
            _exit(0);
        }
        if (clone_into > 0) {
            (void)waitpid(clone_into, NULL, 0);
        }
        close(clone_into_fd);
        errno = clone_into_errno;
        CHECK(clone_into == -1 && clone_into_errno == EAGAIN,
              "clone3 target pids.max rejects child with EAGAIN");
        CHECK(clone_into_parent_tid == -1,
              "denied clone3 leaves the parent TID pointer unchanged");
    }
    CHECK(read_number(CLONE_INTO_PATH "/pids.current") == 0,
          "denied clone3 leaves target pids.current unchanged");
    CHECK(read_number(CLONE_INTO_PATH "/pids.peak") == 0,
          "denied clone3 at the target limit leaves pids.peak unchanged");
    expect_text(CLONE_INTO_PATH "/pids.events", "max 1\n",
                "clone3 target records its own limit failure");
    CHECK(rmdir(CLONE_INTO_PATH) == 0,
          "remove empty clone3 target cgroup");

    CHECK(mkdir(NESTED_PARENT_PATH, 0755) == 0 || errno == EEXIST,
          "create nested pids parent");
    CHECK(write_text(NESTED_PARENT_PATH "/cgroup.subtree_control", "+pids") == 0,
          "enable pids for nested children");
    CHECK(mkdir(NESTED_CHILD_PATH, 0755) == 0 || errno == EEXIST,
          "create nested pids child");
    CHECK(access(NESTED_CHILD_PATH "/pids.max", F_OK) == 0,
          "nested child exposes pids interface");
    CHECK(write_text(NESTED_PARENT_PATH "/pids.max", "1") == 0,
          "set ancestor pids.max to one task");
    CHECK(write_text(NESTED_CHILD_PATH "/cgroup.procs", "0") == 0,
          "migrate current process into nested child");
    CHECK(read_number(NESTED_PARENT_PATH "/pids.current") == 1,
          "ancestor pids.current includes leaf task");
    CHECK(read_number(NESTED_CHILD_PATH "/pids.current") == 1,
          "leaf pids.current includes current task");

    errno = 0;
    pid_t nested_fork = fork();
    int nested_fork_errno = errno;
    CHECK(nested_fork == -1 && nested_fork_errno == EAGAIN,
          "ancestor pids.max rejects a nested fork with EAGAIN");
    if (nested_fork > 0) {
        (void)waitpid(nested_fork, NULL, 0);
    }
    CHECK(read_number(NESTED_PARENT_PATH "/pids.current") == 1,
          "ancestor charge remains stable after denied nested fork");
    CHECK(read_number(NESTED_CHILD_PATH "/pids.current") == 1,
          "leaf charge rolls back after ancestor rejects fork");
    CHECK(read_number(NESTED_PARENT_PATH "/pids.peak") == 1,
          "ancestor rejection does not raise ancestor pids.peak");
    CHECK(read_number(NESTED_CHILD_PATH "/pids.peak") == 2,
          "leaf pids.peak retains the rolled-back hierarchical charge");
    expect_text(NESTED_PARENT_PATH "/pids.events", "max 1\n",
                "ancestor records the denied nested fork");
    expect_text(NESTED_CHILD_PATH "/pids.events", "max 0\n",
                "leaf does not record a limit event after rollback");
    CHECK(write_text(NESTED_PARENT_PATH "/pids.max", "max") == 0,
          "remove the ancestor pids limit");
    CHECK(write_text(NESTED_CHILD_PATH "/pids.max", "1") == 0,
          "set the leaf pids limit to its current task count");

    errno = 0;
    pid_t leaf_fork = fork();
    int leaf_fork_errno = errno;
    CHECK(leaf_fork == -1 && leaf_fork_errno == EAGAIN,
          "leaf pids.max rejects a nested fork with EAGAIN");
    if (leaf_fork > 0) {
        (void)waitpid(leaf_fork, NULL, 0);
    }
    expect_text(NESTED_PARENT_PATH "/pids.events", "max 2\n",
                "ancestor events include a descendant limit failure");
    expect_text(NESTED_CHILD_PATH "/pids.events", "max 1\n",
                "leaf records its own limit failure");
    CHECK(write_text(CGROUP2_PATH "/cgroup.procs", "0") == 0,
          "echo 0 returns nested process to root");
    CHECK(read_number(NESTED_PARENT_PATH "/pids.current") == 0,
          "migration back clears ancestor pids charge");
    CHECK(read_number(NESTED_CHILD_PATH "/pids.current") == 0,
          "migration back clears leaf pids charge");
    CHECK(read_number(NESTED_PARENT_PATH "/pids.peak") == 1,
          "migration back preserves ancestor pids.peak");
    CHECK(read_number(NESTED_CHILD_PATH "/pids.peak") == 2,
          "migration back preserves leaf pids.peak");

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", pass_count, fail_count);
    return fail_count == 0 ? 0 : 1;
}
