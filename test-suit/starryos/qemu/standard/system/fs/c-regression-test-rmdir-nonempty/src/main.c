#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define TEST_DIR "/root/starry-rmdir-nonempty"
#define TEST_FILE TEST_DIR "/child"

static int fail(const char *msg)
{
    printf("FAIL: %s: errno=%d (%s)\n", msg, errno, strerror(errno));
    return 1;
}

int main(void)
{
    unlink(TEST_FILE);
    rmdir(TEST_DIR);

    if (mkdir(TEST_DIR, 0755) != 0) {
        return fail("mkdir test directory");
    }

    int fd = open(TEST_FILE, O_WRONLY | O_CREAT | O_EXCL, 0644);
    if (fd < 0) {
        rmdir(TEST_DIR);
        return fail("create child file");
    }
    if (write(fd, "x", 1) != 1) {
        close(fd);
        unlink(TEST_FILE);
        rmdir(TEST_DIR);
        return fail("write child file");
    }
    close(fd);

    errno = 0;
    if (rmdir(TEST_DIR) == 0) {
        printf("FAIL: rmdir unexpectedly removed a non-empty directory\n");
        return 1;
    }
    if (errno != ENOTEMPTY) {
        return fail("rmdir non-empty directory should fail with ENOTEMPTY");
    }

    if (access(TEST_FILE, F_OK) != 0) {
        return fail("child file should remain after failed rmdir");
    }

    if (unlink(TEST_FILE) != 0) {
        return fail("cleanup child file");
    }
    if (rmdir(TEST_DIR) != 0) {
        return fail("cleanup test directory");
    }

    printf("RMDIR_NONEMPTY_TEST_PASSED\n");
    return 0;
}
