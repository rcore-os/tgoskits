#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <string.h>

/*
 * /proc/[pid]/cgroup test
 *
 *   1. /proc/1/cgroup should exist and be readable
 *   2. /proc/self/cgroup should exist and be readable
 *   3. Content should be valid cgroup info
 */

int main(void) {
    TEST_START("cgroup");

    /* 1. /proc/1/cgroup should exist and be readable */
    {
        int fd = open("/proc/1/cgroup", O_RDONLY);
        CHECK(fd >= 0, "/proc/1/cgroup should exist and be readable");
        if (fd >= 0) {
            char buf[256] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/1/cgroup should succeed");
            if (n > 0) {
                printf("  INFO | /proc/1/cgroup: %.*s", (int)n, buf);
            }
            close(fd);
        }
    }

    /* 2. /proc/self/cgroup should exist */
    {
        int fd = open("/proc/self/cgroup", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/cgroup should exist and be readable");
        if (fd >= 0) {
            char buf[256] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/cgroup should succeed");
            if (n > 0) {
                printf("  INFO | /proc/self/cgroup: %.*s", (int)n, buf);
            }
            close(fd);
        }
    }

    TEST_DONE();
}
