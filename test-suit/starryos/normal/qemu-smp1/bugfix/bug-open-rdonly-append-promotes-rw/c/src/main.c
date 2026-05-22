/*
 * bug-open-rdonly-append-promotes-rw: O_RDONLY|O_APPEND must NOT make the fd
 * writable. APPEND is a status flag and only affects WRITE behavior; it
 * doesn't grant write access.
 *
 * man 2 open §"O_APPEND":
 *   "The file is opened in append mode. Before each write(2), the file offset
 *    is positioned at the end of the file..."
 *   — APPEND is purely about write-time offset; combined with RDONLY it's
 *   effectively a no-op (no writes will happen).
 *
 * Linux behavior: open(file, O_RDONLY|O_APPEND); write(fd,...) → -1 EBADF
 * StarryOS bug: axfs-ng/highlevel/file.rs:to_flags() promotes
 *   (read=true, write=false, append=true) → READ|WRITE|APPEND, so the fd
 *   actually becomes writable.
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
    const char *file = "/tmp/bug_rdonly_append";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "hi", 2);
    close(fd0);

    int fd = open(file, O_RDONLY | O_APPEND);
    if (fd < 0) {
        printf("FAIL: open(file, O_RDONLY|O_APPEND) -> -1 errno=%d (%s); expected fd>=0\n",
               errno, strerror(errno));
        unlink(file);
        return 1;
    }

    /* read should succeed (RDONLY) */
    char buf[8] = {0};
    ssize_t r = read(fd, buf, sizeof(buf) - 1);
    int read_ok = (r == 2 && memcmp(buf, "hi", 2) == 0);

    /* write MUST fail with EBADF (RDONLY) */
    errno = 0;
    ssize_t w = write(fd, "X", 1);
    int write_ok = (w == -1 && errno == EBADF);

    int ok = read_ok && write_ok;
    if (ok) {
        printf("PASS: O_RDONLY|O_APPEND fd is read-only (read works, write -> EBADF)\n");
    } else {
        printf("FAIL: read=%zd (want 2 + content 'hi') write=%zd errno=%d (%s) (want -1 EBADF)\n",
               r, w, errno, strerror(errno));
    }

    close(fd);
    unlink(file);
    return ok ? 0 : 1;
}
