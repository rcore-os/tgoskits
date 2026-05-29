#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>

/*
 * /proc/self/gid_map test
 *
 *   1. /proc/self/gid_map should exist and be readable
 *   2. Writing "0 <gid> 1" maps GID 0 to the specified GID
 *   3. After unshare(CLONE_NEWUSER), gid_map should still exist
 */

static int call_unshare(int flags) {
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void) {
    TEST_START("gid_map");

    /* 1. /proc/self/gid_map should exist */
    {
        int fd = open("/proc/self/gid_map", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/gid_map should exist and be readable");
        if (fd >= 0) {
            char buf[256] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/gid_map should succeed");
            close(fd);
        }
    }

    /* 2. Write gid mapping: "0 0 1" */
    {
        int fd = open("/proc/self/gid_map", O_WRONLY);
        CHECK(fd >= 0, "/proc/self/gid_map should be writable");
        if (fd >= 0) {
            const char *mapping = "0 0 1\n";
            ssize_t n = write(fd, mapping, strlen(mapping));
            CHECK(n > 0 || (n == -1 && (errno == EPERM || errno == EINVAL)),
                  "write to gid_map should succeed or fail with EPERM/EINVAL");
            if (n > 0) {
                printf("  INFO | wrote to gid_map: %.*s\n", (int)n, mapping);
            }
            close(fd);
        }
    }

    /* 3. After unshare(CLONE_NEWUSER), gid_map should still exist */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");
        if (ret == 0) {
            int fd = open("/proc/self/gid_map", O_RDONLY);
            CHECK(fd >= 0, "/proc/self/gid_map should exist after unshare");
            if (fd >= 0) {
                char buf[256] = {0};
                ssize_t n = read(fd, buf, sizeof(buf) - 1);
                CHECK(n >= 0, "read gid_map after unshare should succeed");
                close(fd);
            }
        }
    }

    TEST_DONE();
}
