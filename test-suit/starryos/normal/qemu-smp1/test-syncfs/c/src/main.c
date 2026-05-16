#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <sys/syscall.h>

#ifndef SYS_xsyncfs
#define SYS_xsyncfs 267 /* x86_64 */
#endif

static int xxsyncfs(int fd) {
#if defined(__NR_xsyncfs) || defined(SYS_xsyncfs)
    return syscall(SYS_xsyncfs, fd);
#else
    errno = ENOSYS;
    return -1;
#endif
}

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __fail++;                                                       \
    }                                                                   \
    fflush(stdout);                                                     \
} while(0)

int main(void) {
    printf("================================================\n");
    printf("  TEST: xsyncfs edge cases\n");
    printf("  FILE: %s\n", __FILE__);
    printf("================================================\n");
    fflush(stdout);

    const char *tmpfile = "/tmp/xsyncfs_test.bin";
    unlink(tmpfile);

    /* ---- T1: xsyncfs with valid fd returns 0 ---- */
    printf("\n--- T1: xsyncfs(valid fd) returns 0 ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            const char *data = "hello xsyncfs";
            ssize_t n = write(fd, data, strlen(data));
            CHECK(n == (ssize_t)strlen(data), "write data");

            errno = 0;
            int rc = xsyncfs(fd);
            CHECK(rc == 0, "xsyncfs(valid fd) returns 0");

            close(fd);
        }
    }

    /* ---- T2: xsyncfs with invalid fd returns EBADF ---- */
    printf("\n--- T2: xsyncfs(invalid fd) returns EBADF ---\n"); fflush(stdout);
    {
        errno = 0;
        int rc = xsyncfs(-1);
        CHECK(rc == -1 && errno == EBADF, "xsyncfs(-1) returns EBADF");

        errno = 0;
        rc = xsyncfs(9999);
        CHECK(rc == -1 && errno == EBADF, "xsyncfs(9999) returns EBADF");
    }

    /* ---- T3: xsyncfs after multiple writes ---- */
    printf("\n--- T3: xsyncfs after multiple writes ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            for (int i = 0; i < 10; i++) {
                char buf[64];
                int len = snprintf(buf, sizeof(buf), "line %d\n", i);
                write(fd, buf, len);
            }

            errno = 0;
            int rc = xsyncfs(fd);
            CHECK(rc == 0, "xsyncfs after 10 writes returns 0");

            close(fd);
        }
    }

    /* ---- T4: xsyncfs with read-only fd ---- */
    printf("\n--- T4: xsyncfs(read-only fd) ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDONLY);
        CHECK(fd >= 0, "open read-only");
        if (fd >= 0) {
            errno = 0;
            int rc = xsyncfs(fd);
            CHECK(rc == 0, "xsyncfs(read-only fd) returns 0");

            close(fd);
        }
    }

    /* ---- T5: xsyncfs on two fds of the same file ---- */
    printf("\n--- T5: xsyncfs on two fds of same file ---\n"); fflush(stdout);
    {
        int fd1 = open(tmpfile, O_RDWR);
        int fd2 = open(tmpfile, O_RDWR);
        CHECK(fd1 >= 0, "open fd1");
        CHECK(fd2 >= 0, "open fd2");
        if (fd1 >= 0 && fd2 >= 0) {
            write(fd1, "aaa", 3);
            errno = 0;
            int rc = xsyncfs(fd1);
            CHECK(rc == 0, "xsyncfs(fd1) returns 0");

            write(fd2, "bbb", 3);
            errno = 0;
            rc = xsyncfs(fd2);
            CHECK(rc == 0, "xsyncfs(fd2) returns 0");

            close(fd1);
            close(fd2);
        }
    }

    /* ---- T6: xsyncfs with closed fd returns EBADF ---- */
    printf("\n--- T6: xsyncfs(closed fd) returns EBADF ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR);
        CHECK(fd >= 0, "open file");
        if (fd >= 0) {
            close(fd);
            errno = 0;
            int rc = xsyncfs(fd);
            CHECK(rc == -1 && errno == EBADF, "xsyncfs(closed fd) returns EBADF");
        }
    }

    unlink(tmpfile);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n\n");
    fflush(stdout);

    return __fail > 0 ? 1 : 0;
}
