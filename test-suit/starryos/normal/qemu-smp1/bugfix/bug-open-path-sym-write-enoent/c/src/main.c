/*
 * bug-open-path-sym-write-enoent: O_PATH on a symlink-to-file should follow
 * the symlink and open the target (PATH ignores access mode).
 *
 * man 2 open §"O_PATH":
 *   "[O_PATH] Obtain a file descriptor that can be used for two purposes...
 *    flag bits other than O_CLOEXEC, O_DIRECTORY, and O_NOFOLLOW are ignored."
 *
 * Linux behavior: open(symlink_to_existing_file, O_PATH|O_WRONLY) → fd>=0,
 *   pointing at the target file (symlink followed; access mode ignored).
 * StarryOS bug: returns -1 ENOENT — starry's path/access handling rejects the
 *   combo before honoring O_PATH.
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
    const char *file = "/tmp/bug_path_sym_target";
    const char *sym  = "/tmp/bug_path_sym";
    unlink(sym); unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup file"); return 1; }
    write(fd0, "x", 1);
    close(fd0);
    if (symlink(file, sym) != 0) { perror("setup symlink"); return 1; }

    errno = 0;
    int fd = open(sym, O_PATH | O_WRONLY);

    int ok;
    if (fd >= 0) {
        printf("PASS: open(sym_to_file, O_PATH|O_WRONLY) -> fd=%d\n", fd);
        close(fd);
        ok = 1;
    } else {
        printf("FAIL: expected fd>=0, got -1 errno=%d (%s)\n",
               errno, strerror(errno));
        ok = 0;
    }

    unlink(sym);
    unlink(file);
    return ok ? 0 : 1;
}
