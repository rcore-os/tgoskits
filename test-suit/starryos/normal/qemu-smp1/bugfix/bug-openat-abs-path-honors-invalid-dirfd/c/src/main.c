/*
 * bug-openat-abs-path-honors-invalid-dirfd: openat with an ABSOLUTE pathname
 * must ignore dirfd entirely (per man), even when dirfd is invalid.
 *
 * man 2 open §"openat()":
 *   "If pathname is absolute, then dirfd is ignored."
 *
 * Linux behavior: openat(-1, "/tmp/anything", O_RDONLY) → fd>=0 (dirfd ignored).
 * StarryOS bug: kernel/src/file/fs.rs::with_fs unconditionally calls
 *   Directory::from_fd(dirfd) when dirfd != AT_FDCWD, before path is even
 *   inspected — so invalid dirfd fails with EBADF before the absolute-path
 *   shortcut can take effect. Fix template = sys_fchownat (ctl.rs:413-418,
 *   PR #588) or resolve_at (file/fs.rs:70-75, PR #605):
 *     `let dirfd = if path.starts_with('/') { AT_FDCWD } else { dirfd };`
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
    /* setup: absolute target file */
    const char *file = "/tmp/bug_openat_abs_dirfd";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "x", 1);
    close(fd0);

    int ok = 1;

    /* probe 1: invalid dirfd -1 + absolute path */
    errno = 0;
    int fd = openat(-1, file, O_RDONLY);
    if (fd >= 0) {
        printf("PASS: openat(-1, /abs/path) -> fd=%d (dirfd ignored)\n", fd);
        close(fd);
    } else {
        printf("FAIL: openat(-1, /abs/path) -> -1 errno=%d (%s); expected fd>=0 (dirfd should be ignored)\n",
               errno, strerror(errno));
        ok = 0;
    }

    /* probe 2: non-existent dirfd 9999 + absolute path */
    errno = 0;
    fd = openat(9999, file, O_RDONLY);
    if (fd >= 0) {
        printf("PASS: openat(9999, /abs/path) -> fd=%d (dirfd ignored)\n", fd);
        close(fd);
    } else {
        printf("FAIL: openat(9999, /abs/path) -> -1 errno=%d (%s); expected fd>=0\n",
               errno, strerror(errno));
        ok = 0;
    }

    unlink(file);
    return ok ? 0 : 1;
}
