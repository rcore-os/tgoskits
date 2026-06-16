/*
 * bug-open-path-creat-creates: O_PATH does not create files (O_CREAT ignored).
 *
 * man 2 open §"O_PATH":
 *   "When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
 *    O_DIRECTORY, and O_NOFOLLOW are ignored."
 *
 * Therefore O_PATH|O_CREAT on an absent path should NOT create the file —
 * since O_CREAT is one of the ignored flags. Linux: returns -1 ENOENT
 * (path doesn't exist, can't open).
 *
 * StarryOS bug: returns a valid fd AND creates a real file at the path.
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
    const char *path = "/tmp/bug_path_creat_creates_xyz";
    unlink(path);

    errno = 0;
    int fd = open(path, O_PATH | O_CREAT, 0644);

    int ok;
    struct stat st;
    int file_was_created = (stat(path, &st) == 0);

    if (fd == -1 && errno == ENOENT && !file_was_created) {
        printf("PASS: open(absent, O_PATH|O_CREAT) -> -1 ENOENT, no file created\n");
        ok = 1;
    } else {
        printf("FAIL: expected -1 ENOENT (no creation), got fd=%d errno=%d (%s); file_present_after=%d\n",
               fd, errno, strerror(errno), file_was_created);
        ok = 0;
    }

    if (fd >= 0) close(fd);
    unlink(path);
    return ok ? 0 : 1;
}
