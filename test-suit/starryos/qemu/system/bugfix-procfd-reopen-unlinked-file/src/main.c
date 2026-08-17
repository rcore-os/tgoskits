#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *const FIXTURE = "/tmp/procfd-reopen-unlinked-file";

static int fail(const char *stage)
{
    printf("FAIL: %s errno=%d (%s)\n", stage, errno, strerror(errno));
    return 1;
}

int main(void)
{
    char procfd_path[64];
    char byte = '\0';
    int fd;
    int path_fd;
    int reopened_fd;

    setvbuf(stdout, NULL, _IONBF, 0);
    puts("STARRY_SYSTEM_TEST_BEGIN: bugfix-procfd-reopen-unlinked-file");

    fd = open(FIXTURE, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
    if (fd < 0 || write(fd, "M", 1) != 1 || close(fd) < 0) {
        return fail("create fixture");
    }

    path_fd = (int)syscall(
        SYS_openat,
        AT_FDCWD,
        FIXTURE,
        O_PATH | O_NOFOLLOW | O_CLOEXEC,
        0
    );
    if (path_fd < 0) {
        return fail("pin fixture with O_PATH");
    }
    if (unlink(FIXTURE) < 0) {
        close(path_fd);
        return fail("unlink pinned fixture");
    }

    if (snprintf(procfd_path, sizeof(procfd_path), "/proc/self/fd/%d", path_fd) < 0) {
        close(path_fd);
        return fail("format procfd path");
    }
    reopened_fd = open(procfd_path, O_RDONLY | O_CLOEXEC);
    if (reopened_fd < 0) {
        close(path_fd);
        return fail("reopen unlinked O_PATH file through procfd");
    }
    if (read(reopened_fd, &byte, sizeof(byte)) != 1 || byte != 'M') {
        close(reopened_fd);
        close(path_fd);
        return fail("read procfd-reopened file");
    }

    close(reopened_fd);
    close(path_fd);
    puts("PASS: procfd reopens an unlinked O_PATH regular file");
    return 0;
}
