/*
 * bug-open-dir-wronly: Opening a directory with O_WRONLY should fail with EISDIR.
 *
 * Linux behavior: open("/tmp", O_WRONLY) returns -1 with errno=EISDIR.
 * StarryOS bug: Returns a valid fd instead of failing.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void)
{
    printf("=== bug-open-dir-wronly ===\n");
    printf("Expected: open(\"/tmp\", O_WRONLY) fails with EISDIR\n\n");

    errno = 0;
    int fd = open("/tmp", O_WRONLY);

    if (fd == -1 && errno == EISDIR) {
        printf("PASS: open returned -1, errno=EISDIR (%d)\n", errno);
        printf("TEST PASSED\n");
        return 0;
    }

    if (fd >= 0) {
        printf("FAIL: open returned fd=%d (should have failed)\n", fd);
        close(fd);
    } else {
        printf("FAIL: open returned -1 but errno=%d (%s), expected EISDIR (%d)\n",
               errno, strerror(errno), EISDIR);
    }
    printf("TEST FAILED\n");
    return 1;
}
