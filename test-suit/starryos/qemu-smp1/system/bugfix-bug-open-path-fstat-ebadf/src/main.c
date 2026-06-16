/*
 * bug-open-path-fstat-ebadf: fstat on an O_PATH fd must succeed.
 *
 * man 2 open §"O_PATH" lists fstat(2) (since Linux 3.6) as one of the
 * operations explicitly allowed on O_PATH file descriptors.
 *
 * Linux behavior: fstat on O_PATH fd returns 0, st_size populated.
 * StarryOS bug: returns -1 EBADF — over-zealous "no I/O" rejection in
 *   File::stat / fd_ops Path-handle wrapper.
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
    const char *file = "/tmp/bug_path_fstat";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "12345", 5);
    close(fd0);

    int fd = open(file, O_PATH);
    if (fd < 0) {
        printf("FAIL: open(file, O_PATH) -> -1 errno=%d (%s)\n", errno, strerror(errno));
        unlink(file);
        return 1;
    }

    struct stat st;
    errno = 0;
    int rc = fstat(fd, &st);
    int ok;
    if (rc == 0 && st.st_size == 5) {
        printf("PASS: fstat(O_PATH fd) -> 0 with st_size=5\n");
        ok = 1;
    } else {
        printf("FAIL: fstat returned %d errno=%d (%s) st_size=%ld; expected 0 with size=5\n",
               rc, errno, strerror(errno), (long)st.st_size);
        ok = 0;
    }

    close(fd);
    unlink(file);
    return ok ? 0 : 1;
}
