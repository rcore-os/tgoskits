/*
 * bug-open-tmpfile-no-einval: O_TMPFILE without O_RDWR/O_WRONLY must EINVAL.
 *
 * man 2 open §"O_TMPFILE":
 *   "O_TMPFILE must be specified with one of O_RDWR or O_WRONLY..."
 * Implication: O_TMPFILE | O_RDONLY → EINVAL.
 *
 * StarryOS bug: kernel `flags_to_options` does not recognize O_TMPFILE bits at
 *   all (silently ignored). The call falls through to opening the directory
 *   as RDONLY → returns valid fd. No EINVAL.
 *
 * Linux behavior: -1 EINVAL.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef O_TMPFILE
#define O_TMPFILE 020000000
#endif

int main(void)
{
    const char *dir = "/tmp/bug_tmpfile_einval_dir";
    rmdir(dir);
    if (mkdir(dir, 0755) != 0) { perror("setup"); return 1; }

    errno = 0;
    int fd = open(dir, O_TMPFILE | O_RDONLY, 0644);
    int ok = (fd == -1 && errno == EINVAL);
    if (ok) {
        printf("PASS: open(dir, O_TMPFILE|O_RDONLY) -> -1 EINVAL\n");
    } else {
        printf("FAIL: expected -1 EINVAL, got fd=%d errno=%d (%s)\n",
               fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);
    rmdir(dir);
    return ok ? 0 : 1;
}
