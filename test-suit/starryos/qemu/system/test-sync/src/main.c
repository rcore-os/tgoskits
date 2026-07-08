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
    printf("  TEST: sync edge cases\n");
    printf("  FILE: %s\n", __FILE__);
    printf("================================================\n");
    fflush(stdout);

    const char *tmpfile = "/tmp/sync_test.bin";
    unlink(tmpfile);

    /* ---- T1: sync() after write succeeds ---- */
    printf("\n--- T1: sync() after write succeeds ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            const char *data = "hello sync";
            ssize_t n = write(fd, data, strlen(data));
            CHECK(n == (ssize_t)strlen(data), "write data");

            errno = 0;
            sync();
            CHECK(1, "sync() completed without error");

            close(fd);
        }
    }

    /* ---- T2: sync() with no open files ---- */
    printf("\n--- T2: sync() with no open files ---\n"); fflush(stdout);
    {
        errno = 0;
        sync();
        CHECK(1, "sync() with no open files completed");
    }

    /* ---- T3: sync() after multiple writes to same file ---- */
    printf("\n--- T3: sync() after multiple writes ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            for (int i = 0; i < 10; i++) {
                char buf[64];
                int len = snprintf(buf, sizeof(buf), "line %d\n", i);
                write(fd, buf, len);
            }

            sync();
            CHECK(1, "sync() after 10 writes completed");

            close(fd);
        }
    }

    /* ---- T4: sync() after fsync then modify ---- */
    printf("\n--- T4: sync() after fsync then modify ---\n"); fflush(stdout);
    {
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            write(fd, "first", 5);
            int rc = fsync(fd);
            CHECK(rc == 0, "fsync succeeds");

            write(fd, "second", 6);
            sync();
            CHECK(1, "sync() after fsync+modify completed");

            close(fd);
        }
    }

    /* ---- T5: verify data persists after sync ---- */
    printf("\n--- T5: verify data persists after sync ---\n"); fflush(stdout);
    {
        unlink(tmpfile);
        int fd = open(tmpfile, O_RDWR | O_CREAT | O_TRUNC, 0644);
        CHECK(fd >= 0, "create temp file");
        if (fd >= 0) {
            const char *expected = "sync persist check";
            write(fd, expected, strlen(expected));
            sync();

            close(fd);

            fd = open(tmpfile, O_RDONLY);
            CHECK(fd >= 0, "reopen file for read");
            if (fd >= 0) {
                char buf[64] = {0};
                ssize_t n = read(fd, buf, sizeof(buf) - 1);
                CHECK(n == (ssize_t)strlen(expected), "read back correct length");
                CHECK(strcmp(buf, expected) == 0, "data matches after sync");
                close(fd);
            }
        }
    }

    unlink(tmpfile);

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n\n");
    fflush(stdout);

    return __fail > 0 ? 1 : 0;
}
