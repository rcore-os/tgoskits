/*
 * bug-openat-empty-path-no-enoent: openat with empty path (no AT_EMPTY_PATH)
 * must ENOENT.
 *
 * man 2 open §ENOENT (1st variant): "O_CREAT not set and the named file
 * does not exist." — empty path qualifies as "no such file."
 * AT_EMPTY_PATH is not defined for openat (only some *at functions like
 * fstatat).
 *
 * Linux behavior: openat(AT_FDCWD, "", O_RDONLY) → -1 ENOENT.
 * StarryOS bug: OpenOptions::open via resolve_parent("") returns the CWD
 *   itself; opens it as RDONLY → returns valid fd, no ENOENT.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void)
{
    errno = 0;
    int fd = openat(AT_FDCWD, "", O_RDONLY);
    int ok = (fd == -1 && errno == ENOENT);
    if (ok) {
        printf("PASS: openat(AT_FDCWD, \"\", O_RDONLY) -> -1 ENOENT\n");
    } else {
        printf("FAIL: expected -1 ENOENT, got fd=%d errno=%d (%s)\n",
               fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);
    return ok ? 0 : 1;
}
