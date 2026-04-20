/*
 * bug-pwritev2-read-at: pwritev2 should write data, not read it.
 *
 * Linux behavior: pwritev2 writes iov data to file at offset, returns bytes written.
 * StarryOS bug: sys_pwritev2 calls read_at() instead of write_at(), so it reads
 *               from the file instead of writing to it, returning 0 or garbage.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef SYS_pwritev2
#define SYS_pwritev2 69
#endif

int main(void)
{
    printf("=== bug-pwritev2-read-at ===\n");
    printf("Testing pwritev2: should write data to file, not read it\n\n");

    const char *test_file = "/tmp/pwritev2_test_file";
    unlink(test_file);

    int fd = open(test_file, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        perror("open");
        printf("TEST FAILED\n");
        return 1;
    }

    struct iovec iov[2];
    char buf1[] = "Hello, ";
    char buf2[] = "World!";
    iov[0].iov_base = buf1;
    iov[0].iov_len = sizeof(buf1) - 1;
    iov[1].iov_base = buf2;
    iov[1].iov_len = sizeof(buf2) - 1;

    errno = 0;
    ssize_t ret = syscall(SYS_pwritev2, fd, iov, 2, 0, 0);

    printf("pwritev2 returned: %zd\n", ret);
    printf("Expected: %zu (total bytes written)\n\n", iov[0].iov_len + iov[1].iov_len);

    close(fd);

    if (ret == (ssize_t)(iov[0].iov_len + iov[1].iov_len)) {
        fd = open(test_file, O_RDONLY);
        if (fd >= 0) {
            char read_buf[32] = {0};
            ssize_t n = read(fd, read_buf, sizeof(read_buf) - 1);
            close(fd);
            if (n > 0 && strncmp(read_buf, "Hello, World!", 12) == 0) {
                printf("PASS: pwritev2 wrote correct data: \"%s\"\n", read_buf);
                printf("TEST PASSED\n");
                unlink(test_file);
                return 0;
            } else {
                printf("FAIL: file content mismatch, got: \"%s\"\n", read_buf);
            }
        } else {
            printf("FAIL: could not read back file\n");
        }
    } else {
        printf("FAIL: pwritev2 returned %zd, expected %zu\n",
               ret, iov[0].iov_len + iov[1].iov_len);
        if (errno != 0) {
            printf("errno: %d (%s)\n", errno, strerror(errno));
        }
    }

    unlink(test_file);
    printf("TEST FAILED\n");
    return 1;
}
