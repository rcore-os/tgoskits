/*
 * bug-open-trailing-slash: trailing slash on a non-directory path must fail
 * with ENOTDIR (POSIX requires that a path ending in '/' refers to a directory).
 *
 * Linux behavior:
 *   open("/tmp/regular_file/", O_RDONLY) → -1 ENOTDIR
 *
 * StarryOS bug: axfs-ng-vfs/path.rs:Components::parse_forward strips the
 *   trailing empty component (caused by the '/'); "foo/" becomes "foo" at the
 *   path layer. open() then sees just "foo", opens it as regular file, ignores
 *   the trailing-slash semantic.
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
    const char *file       = "/tmp/bug_trailing_slash_file";
    const char *file_slash = "/tmp/bug_trailing_slash_file/";
    const char *sym        = "/tmp/bug_trailing_slash_sym";
    const char *sym_slash  = "/tmp/bug_trailing_slash_sym/";
    unlink(sym); unlink(file);

    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "x", 1); close(fd0);
    if (symlink(file, sym) != 0) { perror("setup sym"); return 1; }

    int ok = 1;

    errno = 0;
    int fd = open(file_slash, O_RDONLY);
    if (fd == -1 && errno == ENOTDIR) {
        printf("PASS: open(\"file/\", O_RDONLY) -> -1 ENOTDIR\n");
    } else {
        printf("FAIL: open(\"file/\", O_RDONLY) -> fd=%d errno=%d (%s); expected -1 ENOTDIR\n",
               fd, errno, strerror(errno));
        ok = 0;
    }
    if (fd >= 0) close(fd);

    errno = 0;
    fd = open(sym_slash, O_RDONLY);
    if (fd == -1 && errno == ENOTDIR) {
        printf("PASS: open(\"sym_to_file/\", O_RDONLY) -> -1 ENOTDIR\n");
    } else {
        printf("FAIL: open(\"sym_to_file/\", O_RDONLY) -> fd=%d errno=%d (%s); expected -1 ENOTDIR\n",
               fd, errno, strerror(errno));
        ok = 0;
    }
    if (fd >= 0) close(fd);

    unlink(sym); unlink(file);
    return ok ? 0 : 1;
}
