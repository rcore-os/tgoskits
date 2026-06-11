/*
 * bug-open-path-dir-write-eisdir: O_PATH on a directory must succeed regardless
 * of access mode (O_PATH ignores access mode).
 *
 * man 2 open §"O_PATH":
 *   "Opening a file or directory with the O_PATH flag requires no permissions
 *    on the object itself" — and the access mode is ignored for O_PATH fds
 *    (only the path-level operations matter).
 *
 * Linux behavior: open(dir, O_PATH|O_WRONLY) → fd>=0
 * StarryOS bug: returns -1 EISDIR — starry's "writing-a-dir" check fires
 *   before honoring O_PATH semantics.
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
    const char *dir = "/tmp/bug_path_dir_write";
    rmdir(dir);
    if (mkdir(dir, 0755) != 0) { perror("setup mkdir"); return 1; }

    int ok = 1;
    int fd;

    fd = open(dir, O_PATH | O_WRONLY);
    if (fd >= 0) {
        printf("PASS: open(dir, O_PATH|O_WRONLY) -> fd=%d\n", fd);
        close(fd);
    } else {
        printf("FAIL: open(dir, O_PATH|O_WRONLY) -> -1 errno=%d (%s); expected fd>=0\n",
               errno, strerror(errno));
        ok = 0;
    }

    fd = open(dir, O_PATH | O_RDWR);
    if (fd >= 0) {
        printf("PASS: open(dir, O_PATH|O_RDWR) -> fd=%d\n", fd);
        close(fd);
    } else {
        printf("FAIL: open(dir, O_PATH|O_RDWR) -> -1 errno=%d (%s); expected fd>=0\n",
               errno, strerror(errno));
        ok = 0;
    }

    rmdir(dir);
    return ok ? 0 : 1;
}
