/*
 * bug-open-creat-on-existing-dir-no-eisdir: open(existing_dir, O_CREAT|O_RDONLY)
 * must fail with EISDIR.
 *
 * Linux behavior: O_CREAT on a path that already names a directory is rejected
 * with EISDIR (regardless of access mode), because open(2) cannot create
 * directories — and an existing directory cannot be re-opened with CREAT
 * intent.
 *
 * StarryOS bug: returns a valid fd to the directory, ignoring the O_CREAT-vs-
 * directory conflict.
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
    const char *dir = "/tmp/bug_creat_existing_dir";
    rmdir(dir);
    if (mkdir(dir, 0755) != 0) { perror("setup mkdir"); return 1; }

    errno = 0;
    int fd = open(dir, O_CREAT | O_RDONLY, 0644);
    int ok = (fd == -1 && errno == EISDIR);

    if (ok) {
        printf("PASS: open(existing_dir, O_CREAT|O_RDONLY) -> -1 EISDIR\n");
    } else {
        printf("FAIL: expected -1 EISDIR, got fd=%d errno=%d (%s)\n",
               fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);

    rmdir(dir);
    return ok ? 0 : 1;
}
