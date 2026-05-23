/*
 * Test path-family syscalls.
 * 覆盖 mkdirat/getcwd/chdir/unlinkat/rename/linkat/symlinkat/getdents64/fchdir/
 * mknodat/chroot/readlinkat/renameat2
 * 参考 man 2 syscall-name and linux-compatible-testsuit/tests/test_path.c
 */

#include "path_common.h"
#include "tests.h"

int __pass = 0;
int __fail = 0;

static void cleanup(void)
{
    char cmd[256];
    snprintf(cmd, sizeof(cmd), "rm -rf %s", PATH_FAMILY_BASE);
    system(cmd);
}

static void setup(void)
{
    cleanup();
    mkdir(PATH_FAMILY_BASE, 0755);

    char path[256];
    path_join(path, sizeof(path), "regfile");
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) {
        write(fd, "x", 1);
        close(fd);
    }
}

static void teardown(void)
{
    cleanup();
}

int main(void)
{
    TEST_START("path-family: mkdirat/getcwd/chdir/unlinkat/rename/linkat/symlinkat/getdents64/fchdir/mknodat/chroot/readlinkat/renameat2");

    atexit(teardown);
    setup();

    test_mkdirat();
    test_getcwd();
    test_chdir();
    test_unlinkat();
    test_rename();
    test_renameat2();
    test_linkat();
    test_symlinkat_readlinkat();
    test_getdents64();
    test_fchdir();
    test_mknodat();
    test_chroot();

    TEST_DONE();
}
