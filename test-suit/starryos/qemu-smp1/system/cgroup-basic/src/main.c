#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while (0)

#define TEST_START(name)                                                \
    printf("================================================\n");       \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

#define TEST_DONE()                                                     \
    printf("------------------------------------------------\n");       \
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);               \
    printf("================================================\n\n");     \
    return __fail > 0 ? 1 : 0

#define CGROUP2_PATH "/tmp/cg"
#define CGROUP_V1_PATH "/tmp/cg-v1"

static void check_mkdir(const char *path, const char *msg)
{
    errno = 0;
    int ret = mkdir(path, 0755);
    int saved_errno = errno;
    CHECK(ret == 0 || saved_errno == EEXIST, msg);
}

static void expect_mkdir_ok(const char *path, const char *msg)
{
    errno = 0;
    int ret = mkdir(path, 0755);
    CHECK(ret == 0, msg);
}

static void expect_mkdir_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int ret = mkdir(path, 0755);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_rmdir_ok(const char *path, const char *msg)
{
    errno = 0;
    int ret = rmdir(path);
    CHECK(ret == 0, msg);
}

static void expect_rmdir_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int ret = rmdir(path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_open_create_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int fd = open(path, O_CREAT | O_WRONLY, 0644);
    int saved_errno = errno;
    if (fd >= 0) {
        close(fd);
    }
    errno = saved_errno;
    CHECK(fd == -1 && saved_errno == expected_errno, msg);
}

static void expect_open_dir_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int fd = open(path, O_RDONLY | O_DIRECTORY);
    int saved_errno = errno;
    if (fd >= 0) {
        close(fd);
    }
    errno = saved_errno;
    CHECK(fd == -1 && saved_errno == expected_errno, msg);
}

static void expect_path_exists(const char *path, const char *msg)
{
    struct stat st;
    errno = 0;
    int ret = stat(path, &st);
    CHECK(ret == 0, msg);
}

static void expect_path_missing(const char *path, const char *msg)
{
    struct stat st;
    errno = 0;
    int ret = stat(path, &st);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == ENOENT, msg);
}

static ssize_t read_text_file(const char *path, char *buf, size_t cap)
{
    if (cap == 0) {
        errno = EINVAL;
        return -1;
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    errno = 0;
    ssize_t nread = read(fd, buf, cap - 1);
    int saved_errno = errno;
    if (nread >= 0) {
        buf[nread] = '\0';
    }

    close(fd);
    errno = saved_errno;
    return nread;
}

static void expect_empty_file(const char *path, const char *msg)
{
    char buf[16];
    ssize_t nread = read_text_file(path, buf, sizeof(buf));
    CHECK(nread == 0, msg);
}

static int buffer_contains_pid(const char *buf, pid_t pid)
{
    const char *cursor = buf;

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

static void expect_write_errno(const char *path, const char *data,
                               int expected_errno, const char *msg)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        CHECK(0, msg);
        return;
    }

    errno = 0;
    ssize_t written = write(fd, data, strlen(data));
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    CHECK(written == -1 && saved_errno == expected_errno, msg);
}

static void expect_write_ok(const char *path, const char *data, const char *msg)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        CHECK(0, msg);
        return;
    }

    errno = 0;
    ssize_t written = write(fd, data, strlen(data));
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    CHECK(written == (ssize_t)strlen(data), msg);
}

static void expect_file_contains(const char *path, const char *needle,
                                 const char *msg)
{
    char buf[4096];
    ssize_t nread = read_text_file(path, buf, sizeof(buf));
    CHECK(nread >= 0 && strstr(buf, needle) != NULL, msg);
}

static void expect_link_errno(const char *old_path, const char *new_path,
                              int expected_errno, const char *msg)
{
    errno = 0;
    int ret = link(old_path, new_path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_symlink_errno(const char *target, const char *link_path,
                                 int expected_errno, const char *msg)
{
    errno = 0;
    int ret = symlink(target, link_path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_rename_errno(const char *old_path, const char *new_path,
                                int expected_errno, const char *msg)
{
    errno = 0;
    int ret = rename(old_path, new_path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_chdir_ok(const char *path, const char *msg)
{
    errno = 0;
    int ret = chdir(path);
    CHECK(ret == 0, msg);
}

int main(void)
{
    TEST_START("cgroup-basic");

    check_mkdir(CGROUP2_PATH, "mkdir cgroup2 mountpoint");

    errno = 0;
    int ret = mount("none", CGROUP2_PATH, "cgroup2", 0, NULL);
    int mount_errno = errno;
    errno = mount_errno;
    CHECK(ret == 0, "mount cgroup2 succeeds");
    if (ret != 0) {
        printf("  OBSERVE | mount cgroup2 failed errno=%d (%s)\n",
               mount_errno, strerror(mount_errno));
        TEST_DONE();
    }

    char buf[4096];
    ssize_t nread = read_text_file(CGROUP2_PATH "/cgroup.procs", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.procs");
    if (nread >= 0) {
        CHECK(buffer_contains_pid(buf, getpid()),
              "root cgroup.procs contains current process");
    }

    nread = read_text_file(CGROUP2_PATH "/cgroup.controllers", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.controllers");
    if (nread >= 0) {
        CHECK(strstr(buf, "cpu") != NULL && strstr(buf, "memory") != NULL
                  && strstr(buf, "pids") != NULL,
              "root cgroup.controllers exposes mock controllers");
    }

    nread = read_text_file(CGROUP2_PATH "/cgroup.subtree_control", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.subtree_control");
    if (nread >= 0) {
        CHECK(nread == 0, "root cgroup.subtree_control is initially empty");
    }

    expect_write_ok(CGROUP2_PATH "/cgroup.subtree_control",
                    "+cpu +memory +pids",
                    "enable mock controllers in subtree_control");
    expect_file_contains(CGROUP2_PATH "/cgroup.subtree_control", "memory",
                         "subtree_control reports enabled controllers");
    expect_write_errno(CGROUP2_PATH "/cgroup.subtree_control", "+invalid",
                       EINVAL,
                       "enabling an unknown controller fails with EINVAL");

    expect_mkdir_ok(CGROUP2_PATH "/a", "mkdir child cgroup a succeeds");
    expect_path_exists(CGROUP2_PATH "/a", "child cgroup a exists");
    expect_empty_file(CGROUP2_PATH "/a/cgroup.procs",
                      "child cgroup.procs is empty before migration exists");
    expect_file_contains(CGROUP2_PATH "/a/cgroup.controllers", "pids",
                         "child cgroup.controllers exposes mock controllers");
    expect_empty_file(CGROUP2_PATH "/a/cgroup.subtree_control",
                      "child cgroup.subtree_control is initially empty");
    expect_file_contains(CGROUP2_PATH "/a/memory.max", "max",
                         "memory.max has Linux-compatible default");
    expect_write_ok(CGROUP2_PATH "/a/memory.max", "268435456",
                    "write mock memory.max succeeds");
    expect_file_contains(CGROUP2_PATH "/a/memory.max", "268435456",
                         "mock memory.max value can be read back");
    expect_write_ok(CGROUP2_PATH "/a/cpu.max", "50000 100000",
                    "write mock cpu.max succeeds");
    expect_file_contains(CGROUP2_PATH "/a/cpu.max", "50000 100000",
                         "mock cpu.max value can be read back");

    char pid_text[32];
    snprintf(pid_text, sizeof(pid_text), "%ld", (long)getpid());
    expect_write_ok(CGROUP2_PATH "/a/cgroup.procs", pid_text,
                    "move current process into child cgroup");
    nread = read_text_file(CGROUP2_PATH "/a/cgroup.procs", buf, sizeof(buf));
    CHECK(nread >= 0 && buffer_contains_pid(buf, getpid()),
          "child cgroup.procs contains migrated process");
    expect_file_contains("/proc/self/cgroup", "0::/a",
                         "/proc/self/cgroup reports migrated path");

    int ready[2];
    int release[2];
    CHECK(pipe(ready) == 0 && pipe(release) == 0,
          "create synchronization pipes for inheritance test");
    pid_t child = fork();
    if (child == 0) {
        close(ready[0]);
        close(release[1]);
        char marker = 'R';
        if (write(ready[1], &marker, 1) != 1
            || read(release[0], &marker, 1) != 1) {
            _exit(2);
        }
        _exit(0);
    }
    CHECK(child > 0, "fork child for cgroup inheritance");
    if (child > 0) {
        close(ready[1]);
        close(release[0]);
        char marker = 0;
        CHECK(read(ready[0], &marker, 1) == 1,
              "child reaches cgroup inheritance checkpoint");
        nread = read_text_file(CGROUP2_PATH "/a/cgroup.procs", buf, sizeof(buf));
        CHECK(nread >= 0 && buffer_contains_pid(buf, child),
              "fork child inherits parent cgroup");

        expect_write_ok(CGROUP2_PATH "/cgroup.procs", "0",
                        "move current process back to root cgroup");
        expect_file_contains("/proc/self/cgroup", "0::/",
                             "/proc/self/cgroup reports root path");
        marker = 'X';
        CHECK(write(release[1], &marker, 1) == 1,
              "release inherited child");
        int status = 0;
        CHECK(waitpid(child, &status, 0) == child
                  && WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "reap inherited child");
        close(ready[0]);
        close(release[1]);
    }

    expect_mkdir_errno(CGROUP2_PATH "/a", EEXIST,
                       "duplicate mkdir child cgroup fails with EEXIST");

    expect_mkdir_ok(CGROUP2_PATH "/a/b", "mkdir nested child cgroup succeeds");
    expect_rmdir_errno(CGROUP2_PATH "/a", ENOTEMPTY,
                       "rmdir cgroup with child fails with ENOTEMPTY");
    expect_path_exists(CGROUP2_PATH "/a", "parent cgroup remains after failed rmdir");
    expect_path_exists(CGROUP2_PATH "/a/b", "child cgroup remains after failed parent rmdir");
    expect_rmdir_ok(CGROUP2_PATH "/a/b", "rmdir empty nested cgroup succeeds");
    expect_path_missing(CGROUP2_PATH "/a/b", "removed nested cgroup is missing");
    expect_rmdir_ok(CGROUP2_PATH "/a", "rmdir empty child cgroup succeeds");
    expect_path_missing(CGROUP2_PATH "/a", "removed child cgroup is missing");

    expect_mkdir_ok(CGROUP2_PATH "/cwd-cache", "mkdir cgroup for cwd cache regression");
    expect_chdir_ok(CGROUP2_PATH "/cwd-cache", "chdir into cgroup for cwd cache regression");
    expect_mkdir_ok("b", "relative mkdir child cgroup from cwd succeeds");
    expect_rmdir_ok(CGROUP2_PATH "/cwd-cache/b",
                    "absolute rmdir removes child created from cwd");
    expect_open_dir_errno("b", ENOENT,
                          "relative open after absolute rmdir does not return stale cgroup");
    expect_chdir_ok("/", "restore cwd after cwd cache regression");
    expect_rmdir_ok(CGROUP2_PATH "/cwd-cache", "cleanup cwd cache regression cgroup");

    expect_open_create_errno(CGROUP2_PATH "/user.file", EPERM,
                             "open O_CREAT regular file in cgroupfs fails with EPERM");
    expect_path_missing(CGROUP2_PATH "/user.file",
                        "failed open O_CREAT leaves no regular file");
    expect_link_errno(CGROUP2_PATH "/cgroup.procs", CGROUP2_PATH "/procs.link",
                      EPERM, "hard link in cgroupfs fails with EPERM");
    expect_path_missing(CGROUP2_PATH "/procs.link",
                        "failed hard link leaves no new path");
    expect_symlink_errno("x", CGROUP2_PATH "/sym", EPERM,
                         "symlink in cgroupfs fails with EPERM");
    expect_path_missing(CGROUP2_PATH "/sym", "failed symlink leaves no new path");

    expect_mkdir_ok(CGROUP2_PATH "/ren", "mkdir cgroup for rename negative test succeeds");
    expect_rename_errno(CGROUP2_PATH "/ren", CGROUP2_PATH "/renamed", EPERM,
                        "rename cgroup directory fails with EPERM");
    expect_path_exists(CGROUP2_PATH "/ren", "rename failure keeps original cgroup");
    expect_path_missing(CGROUP2_PATH "/renamed", "rename failure leaves destination missing");
    expect_rmdir_ok(CGROUP2_PATH "/ren", "cleanup rename negative test cgroup");

    check_mkdir(CGROUP_V1_PATH, "mkdir cgroup v1 mountpoint");

    errno = 0;
    ret = mount("none", CGROUP_V1_PATH, "cgroup", 0, NULL);
    int v1_errno = errno;
    errno = v1_errno;
    CHECK(ret == -1 && v1_errno == ENODEV, "mount cgroup v1 fails with ENODEV");

    TEST_DONE();
}
