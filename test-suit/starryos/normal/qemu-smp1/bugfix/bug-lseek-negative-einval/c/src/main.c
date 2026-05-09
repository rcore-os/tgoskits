/*
 * bug-lseek-negative-einval: lseek with negative offset from SEEK_SET
 * should fail with EINVAL.
 *
 * Linux behavior: lseek(fd, -1, SEEK_SET) returns -1 with errno=EINVAL.
 * StarryOS bug: Returns -1 but sets errno=EPERM (1) instead of EINVAL (22).
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void)
{
    printf("=== bug-lseek-negative-einval ===\n");
    printf("Expected: lseek(fd, -1, SEEK_SET) fails with EINVAL\n\n");

    int fd = open("/tmp/lseek_test", O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        printf("FAIL: cannot create test file: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }
    write(fd, "data", 4);

    errno = 0;
    off_t pos = lseek(fd, -1, SEEK_SET);

    close(fd);
    unlink("/tmp/lseek_test");

    if (pos == (off_t)-1 && errno == EINVAL) {
        printf("PASS: lseek returned -1, errno=EINVAL (%d)\n", errno);
        printf("TEST PASSED\n");
        return 0;
    }

    if (pos != (off_t)-1) {
        printf("FAIL: lseek returned %ld (should have failed)\n", (long)pos);
    } else {
        printf("FAIL: lseek returned -1 but errno=%d (%s), expected EINVAL (%d)\n",
               errno, strerror(errno), EINVAL);
    }
    printf("TEST FAILED\n");
    return 1;
}
