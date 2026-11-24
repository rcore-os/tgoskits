#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int fails;

static void pass(const char *msg)
{
    printf("  PASS: %s\n", msg);
}

static void fail(const char *msg)
{
    printf("  FAIL: %s (errno=%d: %s)\n", msg, errno, strerror(errno));
    fails++;
}

int main(void)
{
    const char *path = "/tmp/open-unlink-write.tmp";
    const char *payload = "open fd survives unlink\n";
    char buf[64] = {0};

    printf("=== open-unlink-write regression ===\n");
    unlink(path);

    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail("open temp file");
        goto out;
    }
    pass("open temp file");

    if (unlink(path) != 0) {
        fail("unlink open file");
        goto out_close;
    }
    pass("unlink open file");

    int fd2 = open(path, O_RDONLY);
    if (fd2 >= 0 || errno != ENOENT) {
        if (fd2 >= 0) {
            close(fd2);
        }
        fail("path lookup fails after unlink");
        goto out_close;
    }
    pass("path lookup fails after unlink");

    ssize_t n = write(fd, payload, strlen(payload));
    if (n != (ssize_t)strlen(payload)) {
        fail("write through unlinked fd");
        goto out_close;
    }
    pass("write through unlinked fd");

    if (lseek(fd, 0, SEEK_SET) != 0) {
        fail("seek unlinked fd");
        goto out_close;
    }
    pass("seek unlinked fd");

    n = read(fd, buf, sizeof(buf) - 1);
    if (n != (ssize_t)strlen(payload) || strcmp(buf, payload) != 0) {
        fail("read data through unlinked fd");
        goto out_close;
    }
    pass("read data through unlinked fd");

    struct stat st;
    if (fstat(fd, &st) != 0 || st.st_nlink != 0) {
        fail("fstat unlinked fd reports nlink 0");
        goto out_close;
    }
    pass("fstat unlinked fd reports nlink 0");

out_close:
    close(fd);
out:
    printf("\n=== Results: %s ===\n", fails == 0 ? "pass" : "fail");
    if (fails == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    printf("TEST FAILED\n");
    return 1;
}
