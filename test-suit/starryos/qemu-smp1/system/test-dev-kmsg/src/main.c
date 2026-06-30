/*
 * test-dev-kmsg — /dev/kmsg write-side device (char major 1, minor 11).
 *
 * The kernel exposes /dev/kmsg so an early userspace logger (e.g. systemd with
 * log_target=kmsg) can emit records before any journal exists; each write() is
 * one record and its optional <priority> prefix is parsed like Linux's
 * devkmsg_write(). Only the write side exists for now: the read side has no
 * history ring yet and must report EOF rather than an error.
 *
 * This is the kernel-side regression for the /dev/kmsg write path.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

static int write_record(int fd, const char *rec)
{
    size_t len = strlen(rec);
    ssize_t n = write(fd, rec, len);
    if (n != (ssize_t)len) {
        printf("TEST FAILED: write %zd bytes, expected %zu: %s\n", n, len,
               strerror(errno));
        return -1;
    }
    return 0;
}

int main(void)
{
    /* Node identity: a char device at the standard 1:11. */
    struct stat st;
    if (stat("/dev/kmsg", &st) != 0) {
        printf("TEST FAILED: stat /dev/kmsg: %s\n", strerror(errno));
        return EXIT_FAILURE;
    }
    if (!S_ISCHR(st.st_mode)) {
        printf("TEST FAILED: /dev/kmsg is not a character device\n");
        return EXIT_FAILURE;
    }
    if (major(st.st_rdev) != 1 || minor(st.st_rdev) != 11) {
        printf("TEST FAILED: /dev/kmsg rdev is %u:%u, expected 1:11\n",
               major(st.st_rdev), minor(st.st_rdev));
        return EXIT_FAILURE;
    }

    int wfd = open("/dev/kmsg", O_WRONLY);
    if (wfd < 0) {
        printf("TEST FAILED: open /dev/kmsg O_WRONLY: %s\n", strerror(errno));
        return EXIT_FAILURE;
    }

    /* A record with a <priority> prefix, like systemd emits. */
    if (write_record(wfd, "<6>starry kmsg test: prefixed record\n") != 0) {
        close(wfd);
        return EXIT_FAILURE;
    }
    /* A record with no prefix must still be fully consumed. */
    if (write_record(wfd, "starry kmsg test: plain record\n") != 0) {
        close(wfd);
        return EXIT_FAILURE;
    }
    /* A record without a trailing newline is one record too. */
    if (write_record(wfd, "<4>starry kmsg test: no trailing newline") != 0) {
        close(wfd);
        return EXIT_FAILURE;
    }
    close(wfd);

    /* Read side has no history ring yet: read must report EOF, not an error. */
    int rfd = open("/dev/kmsg", O_RDONLY);
    if (rfd < 0) {
        printf("TEST FAILED: open /dev/kmsg O_RDONLY: %s\n", strerror(errno));
        return EXIT_FAILURE;
    }
    char buf[64];
    ssize_t n = read(rfd, buf, sizeof(buf));
    close(rfd);
    if (n != 0) {
        printf("TEST FAILED: read /dev/kmsg returned %zd, expected 0 (EOF)\n", n);
        return EXIT_FAILURE;
    }

    puts("TEST PASSED");
    return EXIT_SUCCESS;
}
