/*
 * bug-open-path-fchmod-bypass: fchmod on an O_PATH fd must fail with EBADF.
 *
 * man 2 open §"O_PATH":
 *   "The file itself is not opened, and other file operations (e.g., read(2),
 *    write(2), fchmod(2), fchown(2), fgetxattr(2), ioctl(2), mmap(2)) fail
 *    with the error EBADF."
 *
 * Linux behavior: fchmod on an O_PATH fd returns -1 EBADF.
 * StarryOS bug: fchmod succeeds, actually changing the file's mode bits —
 *   bypassing the "no real I/O" guarantee of O_PATH.
 *
 * Note on libc behavior: musl's fchmod() falls back to
 * fchmodat(AT_FDCWD, "/proc/self/fd/<n>", mode, 0) on EBADF. On Linux that
 * procfs path also returns EBADF (the procfs symlink to a PATH fd inherits
 * the PATH-only restriction). On starry the kernel must reject both paths
 * to truly match Linux behavior — this test exercises the full libc-visible
 * surface, not the raw syscall.
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
    const char *file = "/tmp/bug_path_fchmod_bypass";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    close(fd0);

    int fd = open(file, O_PATH);
    if (fd < 0) {
        printf("FAIL: open(file, O_PATH) -> -1 errno=%d (%s)\n", errno, strerror(errno));
        unlink(file);
        return 1;
    }

    errno = 0;
    int rc = fchmod(fd, 0600);
    int ok;
    if (rc == -1 && errno == EBADF) {
        printf("PASS: fchmod(O_PATH fd) -> -1 EBADF\n");
        ok = 1;
    } else {
        struct stat st;
        stat(file, &st);
        printf("FAIL: fchmod returned %d errno=%d (%s); file mode now %04o\n",
               rc, errno, strerror(errno), (unsigned)(st.st_mode & 07777));
        ok = 0;
    }

    close(fd);
    unlink(file);
    return ok ? 0 : 1;
}
