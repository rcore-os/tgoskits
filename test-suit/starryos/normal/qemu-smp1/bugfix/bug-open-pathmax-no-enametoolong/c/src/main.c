/*
 * bug-open-pathmax-no-enametoolong: pathname > PATH_MAX must ENAMETOOLONG.
 *
 * man 2 open §"ENAMETOOLONG": "pathname was too long."
 * Linux: open() rejects path lengths > PATH_MAX (4096) with -1 ENAMETOOLONG.
 *
 * StarryOS bug: only checks per-component length (NAME_MAX 255 in axfs-ng-vfs
 *   path.rs). Multi-segment paths totaling > PATH_MAX walk through fine,
 *   eventually hitting "component doesn't exist" → ENOENT instead.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void)
{
    /* 拼出 ~4500 字符的多段路径（每段 50 字符 'b'）*/
    char path[PATH_MAX + 256];
    snprintf(path, sizeof(path), "/tmp");
    while (strlen(path) < PATH_MAX + 100) {
        size_t len = strlen(path);
        if (len + 51 >= sizeof(path)) break;
        path[len] = '/';
        memset(path + len + 1, 'b', 50);
        path[len + 51] = '\0';
    }

    errno = 0;
    int fd = open(path, O_RDONLY);
    int ok = (fd == -1 && errno == ENAMETOOLONG);
    if (ok) {
        printf("PASS: open(path > PATH_MAX) -> -1 ENAMETOOLONG (len=%zu)\n", strlen(path));
    } else {
        printf("FAIL: expected -1 ENAMETOOLONG (len=%zu), got fd=%d errno=%d (%s)\n",
               strlen(path), fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);
    return ok ? 0 : 1;
}
