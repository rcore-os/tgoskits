#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

static ssize_t raw_readlinkat(int dirfd, const char *path, char *buf, size_t bufsiz)
{
    errno = 0;
    return syscall(SYS_readlinkat, dirfd, path, buf, bufsiz);
}

int main(void)
{
    const char *link_path = "/tmp/bug_readlinkat_zero_size_link";
    const char *file_path = "/tmp/bug_readlinkat_zero_size_file";
    const char *target = "readlinkat-zero-target";
    char buf[64];

    unlink(link_path);
    unlink(file_path);

    int fd = open(file_path, O_CREAT | O_WRONLY | O_TRUNC, 0600);
    check(fd >= 0, "create regular file fixture");
    if (fd >= 0) {
        check(write(fd, "x", 1) == 1, "write regular file fixture");
        check(close(fd) == 0, "close regular file fixture");
    }

    check(symlink(target, link_path) == 0, "create symlink fixture");

    memset(buf, 0x5a, sizeof(buf));
    ssize_t ret = raw_readlinkat(AT_FDCWD, link_path, buf, 0);
    check(ret == -1 && errno == EINVAL, "readlinkat symlink with size 0 fails EINVAL");
    check(buf[0] == 0x5a, "size 0 readlinkat leaves destination buffer unchanged");

    ret = raw_readlinkat(AT_FDCWD, link_path, NULL, 0);
    check(ret == -1 && errno == EINVAL, "readlinkat NULL buffer with size 0 fails EINVAL");

    memset(buf, 0, sizeof(buf));
    ret = raw_readlinkat(AT_FDCWD, link_path, buf, 4);
    check(ret == 4, "readlinkat truncates to caller buffer size");
    check(memcmp(buf, target, 4) == 0, "truncated readlinkat bytes match target prefix");

    memset(buf, 0, sizeof(buf));
    ret = raw_readlinkat(AT_FDCWD, link_path, buf, sizeof(buf));
    check(ret == (ssize_t)strlen(target), "readlinkat full-size return is target length");
    if (ret > 0 && ret < (ssize_t)sizeof(buf)) {
        buf[ret] = '\0';
        check(strcmp(buf, target) == 0, "readlinkat full-size bytes match target");
    } else {
        check(0, "readlinkat full-size bytes match target");
    }

    ret = raw_readlinkat(AT_FDCWD, file_path, buf, sizeof(buf));
    check(ret == -1 && errno == EINVAL, "readlinkat regular file fails EINVAL");

    unlink(link_path);
    unlink(file_path);

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
