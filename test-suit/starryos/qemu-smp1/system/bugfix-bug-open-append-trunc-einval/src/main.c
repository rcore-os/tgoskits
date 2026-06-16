/*
 * bug-open-append-trunc-einval: O_RDWR|O_APPEND|O_TRUNC on existing file
 * must succeed (truncate then append).
 *
 * Linux behavior: O_RDWR + O_APPEND + O_TRUNC is a valid combination. open
 *   succeeds; the file is truncated to 0; subsequent writes append.
 * StarryOS bug: axfs-ng/highlevel/file.rs:OpenOptions::is_valid() rejects
 *   `(_, true) => if self.truncate && !self.create_new { return false; }`
 *   when O_APPEND is set with O_TRUNC and CREAT_NEW isn't — returns -1 EINVAL.
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
    const char *file = "/tmp/bug_append_trunc";
    unlink(file);
    int fd0 = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd0 < 0) { perror("setup"); return 1; }
    write(fd0, "hello", 5);
    close(fd0);

    errno = 0;
    int fd = open(file, O_RDWR | O_APPEND | O_TRUNC);
    int ok = 0;
    if (fd >= 0) {
        struct stat st;
        if (stat(file, &st) == 0 && st.st_size == 0) {
            printf("PASS: open(O_RDWR|O_APPEND|O_TRUNC) -> fd=%d, size=0\n", fd);
            ok = 1;
        } else {
            printf("FAIL: opened ok but size=%ld (expected 0)\n", (long)st.st_size);
        }
        close(fd);
    } else {
        printf("FAIL: open(O_RDWR|O_APPEND|O_TRUNC) -> -1 errno=%d (%s); expected fd>=0\n",
               errno, strerror(errno));
    }

    unlink(file);
    return ok ? 0 : 1;
}
