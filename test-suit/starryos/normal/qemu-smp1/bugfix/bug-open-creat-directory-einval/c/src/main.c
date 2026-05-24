/*
 * bug-open-creat-directory-einval: O_CREAT|O_DIRECTORY must fail with EINVAL.
 *
 * man 2 open implies (and Linux enforces): you cannot create a directory with
 * open(); O_CREAT|O_DIRECTORY is rejected up front with EINVAL.
 *
 * Linux behavior: open(<anything>, O_CREAT|O_DIRECTORY) → -1 EINVAL
 * StarryOS bug: doesn't reject the combo; either returns a valid fd (for
 *   existing directories) or lets path-resolution errors fire (ENOTDIR for
 *   regular files, EEXIST when O_EXCL also set on existing path).
 *
 * Root cause: starry's flags_to_options (kernel/src/syscall/fs/fd_ops.rs:26-63)
 *   doesn't validate that CREAT and DIRECTORY are mutually exclusive.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int probe(const char *path, const char *desc)
{
    errno = 0;
    int fd = open(path, O_RDONLY | O_CREAT | O_DIRECTORY, 0644);
    int ok = (fd == -1 && errno == EINVAL);
    if (ok) {
        printf("PASS: %s -> -1 EINVAL\n", desc);
    } else {
        printf("FAIL: %s -> fd=%d errno=%d (%s); expected -1 EINVAL\n",
               desc, fd, errno, strerror(errno));
    }
    if (fd >= 0) close(fd);
    return ok;
}

int main(void)
{
    int ok = 1;

    /* against existing regular file */
    const char *file = "/tmp/bug_creat_dir_file";
    unlink(file);
    int fd = open(file, O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) close(fd);
    ok &= probe(file, "existing regular file + O_CREAT|O_DIRECTORY");
    unlink(file);

    /* against existing directory */
    const char *dir = "/tmp/bug_creat_dir_dir";
    rmdir(dir);
    mkdir(dir, 0755);
    ok &= probe(dir, "existing directory + O_CREAT|O_DIRECTORY");
    rmdir(dir);

    /* against absent path */
    const char *absent = "/tmp/bug_creat_dir_absent_xyz";
    unlink(absent);
    ok &= probe(absent, "absent path + O_CREAT|O_DIRECTORY");
    unlink(absent);

    return ok ? 0 : 1;
}
