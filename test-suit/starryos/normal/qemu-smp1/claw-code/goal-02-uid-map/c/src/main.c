#define _GNU_SOURCE
#include "test_framework.h"
#include <sched.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <string.h>
#include <grp.h>

/*
 * /proc/self/uid_map test
 *
 * After unshare(CLONE_NEWUSER):
 *   1. /proc/self/uid_map should exist and be readable
 *   2. Writing "0 0 1" maps UID 0
 *   3. getuid()/geteuid()/getresuid() return the mapped value (0)
 *   4. Writing uid_map must NOT affect GID side (getgid() stays 65534)
 */

static int call_unshare(int flags) {
    errno = 0;
    return syscall(SYS_unshare, flags);
}

int main(void) {
    TEST_START("uid_map");

    /* 1. /proc/self/uid_map should exist (no unshare needed) */
    {
        int fd = open("/proc/self/uid_map", O_RDONLY);
        CHECK(fd >= 0, "/proc/self/uid_map should exist and be readable");
        if (fd >= 0) {
            char buf[256] = {0};
            ssize_t n = read(fd, buf, sizeof(buf) - 1);
            CHECK(n >= 0, "read /proc/self/uid_map should succeed");
            close(fd);
        }
    }

    /* 2. unshare(CLONE_NEWUSER) then write uid_map + assert getuid/getresuid */
    {
        int ret = call_unshare(CLONE_NEWUSER);
        CHECK(ret == 0, "unshare(CLONE_NEWUSER) should return 0");

        if (ret == 0) {
            /* Before writing uid_map, getuid() should return 65534 (nobody) */
            uid_t uid_before = getuid();
            printf("  INFO | uid before map write: %u\n", uid_before);
            CHECK(uid_before == 65534,
                  "before writing uid_map, getuid() should return 65534 (nobody)");

            /* GID side should also be 65534 */
            gid_t gid_before = getgid();
            printf("  INFO | gid before uid_map write: %u\n", gid_before);
            CHECK(gid_before == 65534,
                  "before writing uid_map, getgid() should return 65534 (nobody)");

            /* Write "0 0 1" to map namespace UID 0 to UID 0 */
            int fd = open("/proc/self/uid_map", O_WRONLY);
            CHECK(fd >= 0, "/proc/self/uid_map should be writable after unshare");
            if (fd >= 0) {
                const char *mapping = "0 0 1\n";
                ssize_t n = write(fd, mapping, strlen(mapping));
                CHECK(n > 0, "write to uid_map should succeed after unshare");
                close(fd);
            }

            /* After writing uid_map, getuid() should return 0 */
            uid_t uid_after = getuid();
            printf("  INFO | uid after map write: %u\n", uid_after);
            CHECK(uid_after == 0,
                  "after writing uid_map \"0 0 1\", getuid() should return 0");

            /* geteuid() should also return 0 */
            uid_t euid_after = geteuid();
            CHECK(euid_after == 0,
                  "after writing uid_map, geteuid() should return 0");

            /* getresuid() should return 0 for all three */
            uid_t ruid, reuid, rsuid;
            CHECK_RET(getresuid(&ruid, &reuid, &rsuid), 0, "getresuid should succeed");
            CHECK(ruid == 0 && reuid == 0 && rsuid == 0,
                  "after writing uid_map, getresuid() should return (0,0,0)");

            /* GID side must NOT be affected by uid_map write */
            gid_t gid_after = getgid();
            printf("  INFO | gid after uid_map write: %u\n", gid_after);
            CHECK(gid_after == 65534,
                  "after writing uid_map, getgid() should STILL return 65534 (not affected by uid_map)");

            /* getegid() should also still return 65534 */
            gid_t egid_after = getegid();
            CHECK(egid_after == 65534,
                  "after writing uid_map, getegid() should STILL return 65534");

            /* getresgid() should still return 65534 for all three */
            gid_t rgid, regid, rsgid;
            CHECK_RET(getresgid(&rgid, &regid, &rsgid), 0, "getresgid should succeed");
            CHECK(rgid == 65534 && regid == 65534 && rsgid == 65534,
                  "after writing uid_map, getresgid() should return (65534,65534,65534)");
        }
    }

    TEST_DONE();
}
