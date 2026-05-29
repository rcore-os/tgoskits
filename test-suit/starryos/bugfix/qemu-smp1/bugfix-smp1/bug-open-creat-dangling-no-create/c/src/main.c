/*
 * bug-open-creat-dangling-no-create: open(dangling_symlink, O_CREAT) (no EXCL)
 * must follow the symlink and create the target file (provided the target's
 * parent directory exists).
 *
 * Linux behavior: open follows the symlink, sees the target doesn't exist,
 *   creates it (parent of target exists), returns valid fd.
 * StarryOS bug: returns -1 ENOENT — symlink resolution doesn't take into
 *   account that O_CREAT should create at the resolved target path.
 *
 * Setup: dangle = symlink → /tmp/<dir>/no  (where /tmp/<dir>/ exists, no
 * doesn't). open(dangle, O_CREAT|O_WRONLY) should create /tmp/<dir>/no.
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
    const char *dir = "/tmp/bug_creat_dangling_dir";
    const char *target = "/tmp/bug_creat_dangling_dir/no";
    const char *sym = "/tmp/bug_creat_dangling_sym";
    unlink(sym);
    unlink(target);
    rmdir(dir);
    if (mkdir(dir, 0755) != 0) { perror("setup mkdir"); return 1; }
    if (symlink(target, sym) != 0) { perror("setup symlink"); rmdir(dir); return 1; }

    errno = 0;
    int fd = open(sym, O_CREAT | O_WRONLY, 0644);
    int ok = 0;
    if (fd >= 0) {
        struct stat st;
        if (stat(target, &st) == 0) {
            printf("PASS: open(dangling, O_CREAT|O_WRONLY) -> fd=%d, target file created\n", fd);
            ok = 1;
        } else {
            printf("FAIL: open returned fd=%d but stat(target) failed: %s\n", fd, strerror(errno));
        }
        close(fd);
    } else {
        printf("FAIL: open(dangling, O_CREAT|O_WRONLY) -> -1 errno=%d (%s); expected fd>=0\n",
               errno, strerror(errno));
    }

    unlink(sym);
    unlink(target);
    rmdir(dir);
    return ok ? 0 : 1;
}
