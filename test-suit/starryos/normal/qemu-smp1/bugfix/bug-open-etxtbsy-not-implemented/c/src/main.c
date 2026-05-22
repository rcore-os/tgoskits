/*
 * bug-open-etxtbsy-not-implemented: open running self binary for write should
 * fail with -1 ETXTBSY, but starry doesn't track text-busy → fd>=0 succeeds.
 *
 * man 2 open §"ETXTBSY":
 *   "ETXTBSY — pathname refers to an executable image which is currently
 *    being executed and write access was requested."
 *
 * Linux behavior (host/WSL2 verified): open(/proc/self/exe, O_WRONLY) → -1 ETXTBSY (errno 26)
 * StarryOS bug: returns valid fd (no deny_write_access tracking on exec'd binary).
 *
 * Note: full Linux implementation requires kernel-side tracking via
 * inode->i_writecount + deny_write_access() on exec, plus get_write_access
 * checks on open. Starry's vfs lacks this entirely.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(void)
{
    /* Use /proc/self/exe symlink (works on both Linux and starry's procfs) */
    errno = 0;
    int fd = open("/proc/self/exe", O_WRONLY);
    int err = errno;
    int ok = (fd == -1 && err == ETXTBSY);
    if (ok) {
        printf("PASS: open(/proc/self/exe, O_WRONLY) -> -1 ETXTBSY\n");
    } else {
        printf("FAIL: expected -1 ETXTBSY, got fd=%d errno=%d (%s)\n",
               fd, err, strerror(err));
    }
    if (fd >= 0) close(fd);
    return ok ? 0 : 1;
}
