/*
 * Test: pwrite64 / pread64 with negative offset must return EINVAL
 *
 * This verifies that StarryOS correctly rejects negative offsets
 * in both sys_pwrite64 and sys_pread64, matching Linux behaviour.
 *
 * Build (native Linux):
 *   gcc -o test_pwrite64_negative_offset tests/test_pwrite64_negative_offset.c
 *
 * Build (cross-compile for StarryOS target):
 *   <cross-compiler>-gcc -o test_pwrite64_negative_offset \
 *       tests/test_pwrite64_negative_offset.c -static
 */

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/stat.h>

#define TMPFILE "/tmp/starry_test_pwrite64"

static int check_pwrite64_negative_offset(void)
{
    int fd;
    ssize_t ret;
    char buf[] = "hello";

    fd = open(TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        perror("open");
        return -1;
    }

    errno = 0;
    ret = pwrite64(fd, buf, sizeof(buf), -1);
    if (ret == -1 && errno == EINVAL) {
        printf("PASS: pwrite64 with negative offset returned EINVAL\n");
        close(fd);
        unlink(TMPFILE);
        return 0;
    }

    printf("FAIL: pwrite64 with negative offset: ret=%zd, errno=%d (%s)\n",
           ret, errno, strerror(errno));
    close(fd);
    unlink(TMPFILE);
    return -1;
}

static int check_pread64_negative_offset(void)
{
    int fd;
    ssize_t ret;
    char buf[16];

    fd = open(TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        perror("open");
        return -1;
    }

    errno = 0;
    ret = pread64(fd, buf, sizeof(buf), -1);
    if (ret == -1 && errno == EINVAL) {
        printf("PASS: pread64 with negative offset returned EINVAL\n");
        close(fd);
        unlink(TMPFILE);
        return 0;
    }

    printf("FAIL: pread64 with negative offset: ret=%zd, errno=%d (%s)\n",
           ret, errno, strerror(errno));
    close(fd);
    unlink(TMPFILE);
    return -1;
}

int main(void)
{
    int failures = 0;

    if (check_pwrite64_negative_offset() < 0)
        failures++;
    if (check_pread64_negative_offset() < 0)
        failures++;

    if (failures == 0)
        printf("\nAll tests passed.\n");
    else
        printf("\n%d test(s) failed.\n", failures);

    return failures;
}
