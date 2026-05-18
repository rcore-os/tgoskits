#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

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
    printf("  TEST: syncfs edge cases\n");
    printf("  FILE: %s\n", __FILE__);
    printf("================================================\n");
    fflush(stdout);

    const char *tmpfile = "/tmp/syncfs_test.bin";
    unlink(tmpfile);

    /* ---- T1: syncfs with valid fd returns 0 ---- */
    printf("\n--- T1: syncfs(valid fd) returns 0 ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            const char *data = "hello syncfs";
            ssize_t n = write(fd, data, strlen(data));
            CHECK(n == (ssize_t)strlen(data), "write data");

            errno = 0;
            int rc = syncfs(fd);
            CHECK(rc == 0, "syncfs(valid fd) returns 0");

            close(fd);
        }
    }

    /* ---- T2: syncfs with invalid fd returns EBADF ---- */
    printf("\n--- T2: syncfs(invalid fd) returns EBADF ---\n"); fflush(stdout);
    {
        errno = 0;
        int rc = syncfs(-1);
        CHECK(rc == -1 && errno == EBADF, "syncfs(-1) returns EBADF");

        errno = 0;
        rc = syncfs(9999);
        CHECK(rc == -1 && errno == EBADF, "syncfs(9999) returns EBADF");
    }

    /* ---- T3: syncfs after multiple writes ---- */
    printf("\n--- T3: syncfs after multiple writes ---\n"); fflush(stdout);
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
            int rc = syncfs(fd);
            CHECK(rc == 0, "syncfs after 10 writes returns 0");

            close(fd);
        }
    }

    /* ---- T4: syncfs with read-only fd ---- */
    printf("\n--- T4: syncfs(read-only fd) ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDONLY);
        CHECK(fd >= 0, "open read-only");
        if (fd >= 0) {
            errno = 0;
            int rc = syncfs(fd);
            CHECK(rc == 0, "syncfs(read-only fd) returns 0");

            close(fd);
        }
    }

    /* ---- T5: syncfs on two fds of the same file ---- */
    printf("\n--- T5: syncfs on two fds of same file ---\n"); fflush(stdout);
    {
        int fd1 = open(tmpfile, O_RDWR);
        int fd2 = open(tmpfile, O_RDWR);
        CHECK(fd1 >= 0, "open fd1");
        CHECK(fd2 >= 0, "open fd2");
        if (fd1 >= 0 && fd2 >= 0) {
            write(fd1, "aaa", 3);
            errno = 0;
            int rc = syncfs(fd1);
            CHECK(rc == 0, "syncfs(fd1) returns 0");

            write(fd2, "bbb", 3);
            errno = 0;
            rc = syncfs(fd2);
            CHECK(rc == 0, "syncfs(fd2) returns 0");

            close(fd1);
            close(fd2);
        }
    }

    /* ---- T6: syncfs with closed fd returns EBADF ---- */
    printf("\n--- T6: syncfs(closed fd) returns EBADF ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR);
        CHECK(fd >= 0, "open file");
        if (fd >= 0) {
            close(fd);
            errno = 0;
            int rc = syncfs(fd);
            CHECK(rc == -1 && errno == EBADF, "syncfs(closed fd) returns EBADF");
        }
    }

    unlink(tmpfile);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n\n");
    fflush(stdout);

    return __fail > 0 ? 1 : 0;
}
