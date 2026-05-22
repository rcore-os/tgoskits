/*
 * bug-open-rdonly-trunc-einval: O_RDONLY|O_TRUNC must not return EINVAL.
 *
 * man 2 open §"VERSIONS":
 *   "O_RDONLY | O_TRUNC — The (undefined) effect of O_RDONLY | O_TRUNC varies
 *    among implementations. On many systems the file is actually truncated."
 *
 * Linux multi-fs default: file IS truncated (open succeeds with fd, file becomes empty).
 * StarryOS bug: OpenOptions::is_valid() (axfs-ng/highlevel/file.rs:289-307)
 *   rejects the combination outright → returns -1 EINVAL.
 *
 * This test asserts Linux behavior (truncate on success). On starry, EINVAL
 * appears instead.
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
    const char *file = "/tmp/bug_rdonly_trunc";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "hello", 5);
    close(fd0);

    errno = 0;
    int fd = open(file, O_RDONLY | O_TRUNC);

    int ok;
    if (fd >= 0) {
        struct stat st;
        if (stat(file, &st) == 0 && st.st_size == 0) {
            printf("PASS: O_RDONLY|O_TRUNC truncated file (size 0)\n");
            ok = 1;
        } else {
            printf("FAIL: O_RDONLY|O_TRUNC opened ok but file size=%ld (expected 0)\n",
                   (long)st.st_size);
            ok = 0;
        }
        close(fd);
    } else {
        printf("FAIL: O_RDONLY|O_TRUNC -> -1 errno=%d (%s); expected truncate to size 0\n",
               errno, strerror(errno));
        ok = 0;
    }

    unlink(file);
    return ok ? 0 : 1;
}
