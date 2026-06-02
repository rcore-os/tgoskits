#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <string.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <fcntl.h>

static int _pass, _fail;

#define T(desc, expr) do { \
    errno = 0; \
    if ((expr) == 0) { \
        printf("PASS: %s\n", desc); _pass++; \
    } else { \
        printf("FAIL: %s (errno=%d %s)\n", desc, errno, strerror(errno)); _fail++; \
    } \
} while(0)

int main() {
    system("rm -rf /tmp/rn");

    /* A: sibling dirs (control - should pass) */
    mkdir("/tmp/rn", 0755);
    mkdir("/tmp/rn/A", 0755); mkdir("/tmp/rn/B", 0755);
    int fd = creat("/tmp/rn/A/f.txt", 0644); write(fd, "x", 1); close(fd);
    T("sibling: A/f → B/f", rename("/tmp/rn/A/f.txt", "/tmp/rn/B/f.txt"));

    /* B: parent → child (git reflog pattern) */
    mkdir("/tmp/rn/parent", 0755);
    mkdir("/tmp/rn/parent/child", 0755);
    fd = creat("/tmp/rn/parent/f.txt", 0644); write(fd, "y", 1); close(fd);
    T("parent→child: parent/f → parent/child/f",
      rename("/tmp/rn/parent/f.txt", "/tmp/rn/parent/child/f.txt"));

    /* C: child → parent */
    fd = creat("/tmp/rn/parent/child/g.txt", 0644); write(fd, "z", 1); close(fd);
    T("child→parent: parent/child/g → parent/g",
      rename("/tmp/rn/parent/child/g.txt", "/tmp/rn/parent/g.txt"));

    /* D: 2-level parent→child (like refs → refs/heads) */
    mkdir("/tmp/rn/top", 0755);
    mkdir("/tmp/rn/top/mid", 0755);
    mkdir("/tmp/rn/top/mid/sub", 0755);
    fd = creat("/tmp/rn/top/mid/f.txt", 0644); write(fd, "w", 1); close(fd);
    T("refs→refs/heads: top/mid/f → top/mid/sub/f",
      rename("/tmp/rn/top/mid/f.txt", "/tmp/rn/top/mid/sub/f.txt"));

    /* E: what if dest parent is created by mkdir in same run */
    system("rm -rf /tmp/rn/test2");
    mkdir("/tmp/rn/test2", 0755);
    mkdir("/tmp/rn/test2/src", 0755);
    fd = creat("/tmp/rn/test2/src/f.txt", 0644); write(fd, "v", 1); close(fd);
    mkdir("/tmp/rn/test2/dst", 0755);
    T("fresh dirs: test2/src/f → test2/dst/f",
      rename("/tmp/rn/test2/src/f.txt", "/tmp/rn/test2/dst/f.txt"));

    system("rm -rf /tmp/rn");
    printf("\nPASS=%d FAIL=%d\n", _pass, _fail);
    return _fail ? 1 : 0;
}
