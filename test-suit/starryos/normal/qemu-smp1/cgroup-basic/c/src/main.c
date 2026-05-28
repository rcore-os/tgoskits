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
    errno = saved_errno;
    CHECK(ret == 0 || saved_errno == EEXIST, msg);
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
        CHECK(nread == 0, "root cgroup.controllers is empty before controllers exist");
    }

    nread = read_text_file(CGROUP2_PATH "/cgroup.subtree_control", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.subtree_control");
    if (nread >= 0) {
        CHECK(nread == 0, "root cgroup.subtree_control is initially empty");
    }

    expect_write_errno(CGROUP2_PATH "/cgroup.subtree_control", "+pids",
                       EINVAL, "writing +pids to subtree_control fails with EINVAL");

    check_mkdir(CGROUP_V1_PATH, "mkdir cgroup v1 mountpoint");

    errno = 0;
    ret = mount("none", CGROUP_V1_PATH, "cgroup", 0, NULL);
    int v1_errno = errno;
    errno = v1_errno;
    CHECK(ret == -1 && v1_errno == ENODEV, "mount cgroup v1 fails with ENODEV");

    TEST_DONE();
}
