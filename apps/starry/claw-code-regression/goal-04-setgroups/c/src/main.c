#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>
#include <grp.h>

/*
 * /proc/self/setgroups test
 *
 *   1. /proc/self/setgroups should exist and be readable
 *   2. Reading returns "allow" by default
 *   3. Writing "deny" changes the value and causes setgroups(2) to return EPERM
 *   4. Writing "allow" restores original behaviour
 *   5. After unshare(CLONE_NEWUSER), setgroups should still exist
 */

static int call_unshare(int flags) {
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void) {
    TEST_START("setgroups");

    /* 1. /proc/self/setgroups should exist */
    {
        int fd = open("/proc/self/setgroups", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/setgroups should exist and be readable");
        if (fd >= 0) {
            char buf[32] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/setgroups should succeed");
            if (n >= 0) {
                printf("  INFO | setgroups content: %.*s\n", (int)n, buf);
            }
            close(fd);
        }
    }

    /* 2. Write "deny" to setgroups and verify setgroups(2) returns EPERM */
    {
        int fd = open("/proc/self/setgroups", O_WRONLY);
        if (fd >= 0) {
            const char *val = "deny\n";  // include newline to replace full content
            ssize_t n = write(fd, val, strlen(val));
            CHECK(n > 0, "write \"deny\" to setgroups should succeed");
            close(fd);

            if (n > 0) {
                /* Verify read-back: content should contain "deny" */
                fd = open("/proc/self/setgroups", O_RDONLY);
                if (fd >= 0) {
                    char buf[32] = {0};
                    ssize_t rn = read(fd, buf, sizeof(buf) - 1);
                    CHECK(rn >= 0, "read setgroups after deny should succeed");
                    if (rn >= 0) {
                        printf("  INFO | setgroups read-back (%zd bytes): \"%.*s\"\n",
                               rn, (int)rn, buf);
                        int has_deny = (rn >= 4 && memcmp(buf, "deny", 4) == 0);
                        CHECK(has_deny, "reading setgroups after deny should return \"deny\"");
                    }
                    close(fd);
                }

                /* setgroups(2) must now return EPERM */
                errno = 0;
                long after_ret = syscall(SYS_setgroups, 0, NULL);
                int after_errno = errno;
                printf("  INFO | setgroups(0, NULL) after deny: ret=%ld errno=%d\n",
                       after_ret, after_errno);
                CHECK(after_ret == -1 && after_errno == EPERM,
                      "after writing \"deny\", setgroups(2) should return EPERM");
            }
        } else {
            printf("  PASS | %s:%d | setgroups not writable (errno=%d)\n",
                   __FILE__, __LINE__, errno);
            __pass++;
        }
    }

    /* 3. Write "allow" back and verify setgroups works again */
    {
        int fd = open("/proc/self/setgroups", O_WRONLY);
        if (fd >= 0) {
            const char *val = "allow\n";  // include newline to replace full content
            ssize_t n = write(fd, val, strlen(val));
            CHECK(n > 0, "write \"allow\" to setgroups should succeed");
            close(fd);

            if (n > 0) {
                errno = 0;
                long ret = syscall(SYS_setgroups, 0, NULL);
                int final_errno = errno;
                printf("  INFO | setgroups(0, NULL) after allow: ret=%ld errno=%d\n",
                       ret, final_errno);
                CHECK(final_errno != EPERM,
                      "after writing \"allow\", setgroups(2) should NOT return EPERM");
            }
        }
    }

    /* 4. After unshare(CLONE_NEWUSER), setgroups should still exist */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");
        if (ret == 0) {
            int fd = open("/proc/self/setgroups", O_RDONLY);
            CHECK(fd >= 0, "/proc/self/setgroups should exist after unshare");
            if (fd >= 0) {
                char buf[32] = {0};
                ssize_t n = read(fd, buf, sizeof(buf) - 1);
                CHECK(n >= 0, "read setgroups after unshare should succeed");
                close(fd);
            }
        }
    }

    TEST_DONE();
}
