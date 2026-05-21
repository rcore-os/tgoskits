/*
 * bug-open-path-honors-excl: O_PATH should ignore O_CREAT/O_EXCL.
 *
 * man 2 open §"O_PATH":
 *   "When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
 *    O_DIRECTORY, and O_NOFOLLOW are ignored."
 *
 * Linux behavior: open(existing_file, O_PATH|O_CREAT|O_EXCL) returns valid fd.
 * StarryOS bug: returns -1 EEXIST (CREAT|EXCL still honored even when PATH set).
 *
 * Root cause: flags_to_options (fd_ops.rs:26-63) sets create/create_new from
 *   O_CREAT/O_EXCL bits unconditionally; the O_PATH branch is only set later
 *   via options.path(true) but doesn't clear/override the create flags.
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
    const char *file = "/tmp/bug_path_excl_file";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "x", 1);
    close(fd0);

    errno = 0;
    int fd = open(file, O_PATH | O_CREAT | O_EXCL, 0644);
    int ok = (fd >= 0);
    if (ok) {
        printf("PASS: open(existing, O_PATH|O_CREAT|O_EXCL) -> fd=%d (PATH ignored CREAT/EXCL)\n", fd);
        close(fd);
    } else {
        printf("FAIL: expected fd>=0 (PATH ignores CREAT/EXCL), got fd=-1 errno=%d (%s)\n",
               errno, strerror(errno));
    }

    unlink(file);
    return ok ? 0 : 1;
}
