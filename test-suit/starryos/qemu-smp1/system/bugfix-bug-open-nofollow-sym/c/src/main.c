/*
 * bug-open-nofollow-sym: O_NOFOLLOW + symlink basename should fail with ELOOP.
 *
 * man 2 open §"O_NOFOLLOW":
 *   "If the trailing component (i.e., basename) of pathname is a symbolic link,
 *    then the open fails, with the error ELOOP."
 *
 * Linux behavior: open(symlink_to_file, O_RDONLY|O_NOFOLLOW) returns -1, errno=ELOOP
 * StarryOS bug: returns a valid fd to the symlink target.
 *
 * Root cause (from source reading):
 *   axfs-ng/highlevel/file.rs:OpenOptions::open() at `if !self.no_follow`
 *   only resolves symlinks when no_follow is false. When no_follow=true, it
 *   skips try_resolve_symlink and returns the raw location — but never
 *   explicitly checks "if loc is symlink → ELOOP". So basename symlinks slip
 *   through and get opened as the symlink (which to the file layer looks like
 *   a regular file, since the kernel reads the link target's data).
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
    const char *file = "/tmp/bug_open_nofollow_target";
    const char *sym  = "/tmp/bug_open_nofollow_sym";

    /* setup: regular file + symlink pointing at it */
    unlink(sym);
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup open"); return 1; }
    write(fd0, "x", 1);
    close(fd0);
    if (symlink(file, sym) != 0) { perror("setup symlink"); return 1; }

    /* the actual probe */
    errno = 0;
    int fd = open(sym, O_RDONLY | O_NOFOLLOW);

    int ok = (fd == -1 && errno == ELOOP);
    if (ok) {
        printf("PASS: open(symlink, O_RDONLY|O_NOFOLLOW) -> -1 ELOOP as expected\n");
    } else {
        printf("FAIL: expected -1 ELOOP, got fd=%d errno=%d (%s)\n",
               fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);

    unlink(sym);
    unlink(file);
    return ok ? 0 : 1;
}
