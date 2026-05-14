#include "test_framework.h"

#include <fcntl.h>
#include <stdint.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *BASE = "/tmp/starry_syscall_test_path_family";

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static void path_join(char *out, size_t out_size, const char *rel)
{
    snprintf(out, out_size, "%s/%s", BASE, rel);
}

static void cleanup(void)
{
    char cmd[256];
    snprintf(cmd, sizeof(cmd), "rm -rf %s", BASE);
    system(cmd);
}

static void setup(void)
{
    cleanup();
    mkdir(BASE, 0755);

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

static int open_base_dir(void)
{
    int dfd = open(BASE, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0, "open(BASE) as dirfd");
    return dfd;
}

static void test_mkdirat_success(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    CHECK_RET(mkdirat(dfd, "d1", 0755), 0, "mkdirat: create d1");

    struct stat st;
    CHECK_RET(fstatat(dfd, "d1", &st, 0), 0, "mkdirat: fstatat(d1)");
    CHECK(S_ISDIR(st.st_mode), "mkdirat: d1 is directory");

    close(dfd);
}

static void test_mkdirat_eexist(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    mkdirat(dfd, "d1", 0755);
    CHECK_ERR(mkdirat(dfd, "d1", 0755), EEXIST, "mkdirat: existing dir -> EEXIST");

    close(dfd);
}

static void test_mkdirat_ebadf(void)
{
    CHECK_ERR(mkdirat(-1, "x", 0755), EBADF, "mkdirat: dirfd=-1 -> EBADF");
}

static void test_mkdirat_enotdir(void)
{
    char path[256];
    path_join(path, sizeof(path), "regfile");
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 0, "mkdirat(ENOTDIR): open regfile");
    if (fd < 0) {
        return;
    }

    CHECK_ERR(mkdirat(fd, "x", 0755), ENOTDIR, "mkdirat: dirfd is file -> ENOTDIR");
    close(fd);
}

static void test_mkdirat_enoent(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    CHECK_ERR(mkdirat(dfd, "no_such_parent/x", 0755), ENOENT, "mkdirat: missing parent -> ENOENT");
    close(dfd);
}

static void test_getcwd_success_and_erange(void)
{
    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "getcwd: capture old cwd");

    CHECK_RET(chdir(BASE), 0, "getcwd: chdir(BASE)");

    char cwd[512];
    CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "getcwd: read cwd");
    CHECK(strcmp(cwd, BASE) == 0, "getcwd: cwd equals BASE");

    char small[2];
    errno = 0;
    CHECK(getcwd(small, sizeof(small)) == NULL && errno == ERANGE, "getcwd: small buffer -> ERANGE");

    chdir(old_cwd);
}

static void test_chdir_success_and_failures(void)
{
    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "chdir: capture old cwd");

    char target[256];
    path_join(target, sizeof(target), "chdir_target");
    mkdir(target, 0755);

    CHECK_RET(chdir(target), 0, "chdir: to existing directory");
    char cwd[512];
    CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "chdir: getcwd after chdir");
    CHECK(strcmp(cwd, target) == 0, "chdir: cwd equals target");

    chdir(old_cwd);
    rmdir(target);

    errno = 0;
    CHECK(chdir("/tmp/starry_syscall_test_path_family/no_such_path") == -1 && errno == ENOENT,
          "chdir: missing path -> ENOENT");

    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");
    errno = 0;
    CHECK(chdir(regfile) == -1 && errno == ENOTDIR, "chdir: regfile -> ENOTDIR");
}

static void test_unlinkat_success_and_failures(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    int fd = openat(dfd, "workfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "unlinkat: create workfile");
    if (fd >= 0) {
        write(fd, "hello", 5);
        close(fd);
    }
    CHECK_RET(unlinkat(dfd, "workfile", 0), 0, "unlinkat: delete file");

    struct stat st;
    errno = 0;
    CHECK(fstatat(dfd, "workfile", &st, 0) == -1 && errno == ENOENT, "unlinkat: deleted file -> ENOENT");

    mkdirat(dfd, "emptydir", 0755);
    CHECK_RET(unlinkat(dfd, "emptydir", AT_REMOVEDIR), 0, "unlinkat: delete empty dir (AT_REMOVEDIR)");
    errno = 0;
    CHECK(fstatat(dfd, "emptydir", &st, 0) == -1 && errno == ENOENT, "unlinkat: deleted dir -> ENOENT");

    mkdirat(dfd, "nonempty", 0755);
    int child = openat(dfd, "nonempty/child", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (child >= 0) {
        close(child);
    }
    CHECK_ERR(unlinkat(dfd, "nonempty", AT_REMOVEDIR), ENOTEMPTY, "unlinkat: non-empty dir -> ENOTEMPTY");
    unlinkat(dfd, "nonempty/child", 0);
    unlinkat(dfd, "nonempty", AT_REMOVEDIR);

    CHECK_ERR(unlinkat(dfd, "no_such_file", 0), ENOENT, "unlinkat: missing file -> ENOENT");
    CHECK_ERR(unlinkat(dfd, "x", 0x80000000u), EINVAL, "unlinkat: invalid flags -> EINVAL");

    close(dfd);
}

static void test_renameat_cross_dir(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    mkdirat(dfd, "dir_a", 0755);
    mkdirat(dfd, "dir_b", 0755);

    int dfd_a = openat(dfd, "dir_a", O_RDONLY | O_DIRECTORY);
    int dfd_b = openat(dfd, "dir_b", O_RDONLY | O_DIRECTORY);
    CHECK(dfd_a >= 0 && dfd_b >= 0, "renameat: open dir_a/dir_b");
    if (dfd_a < 0 || dfd_b < 0) {
        if (dfd_a >= 0) {
            close(dfd_a);
        }
        if (dfd_b >= 0) {
            close(dfd_b);
        }
        unlinkat(dfd, "dir_a", AT_REMOVEDIR);
        unlinkat(dfd, "dir_b", AT_REMOVEDIR);
        close(dfd);
        return;
    }

    int fd = openat(dfd_a, "movefile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "renameat: create dir_a/movefile");
    if (fd >= 0) {
        write(fd, "moved", 5);
        close(fd);
    }

    CHECK_RET(renameat(dfd_a, "movefile", dfd_b, "movedfile"), 0, "renameat: move dir_a -> dir_b");

    struct stat st;
    errno = 0;
    CHECK(fstatat(dfd_a, "movefile", &st, 0) == -1 && errno == ENOENT, "renameat: source missing after move");
    CHECK_RET(fstatat(dfd_b, "movedfile", &st, 0), 0, "renameat: dest exists after move");

    unlinkat(dfd_b, "movedfile", 0);
    close(dfd_a);
    close(dfd_b);
    unlinkat(dfd, "dir_a", AT_REMOVEDIR);
    unlinkat(dfd, "dir_b", AT_REMOVEDIR);
    close(dfd);
}

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE 1
#endif

static void test_renameat2_noreplace_and_invalid_flags(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    int fd = openat(dfd, "srcfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "renameat2: create srcfile");
    if (fd >= 0) {
        write(fd, "renameat2", 9);
        close(fd);
    }

    fd = openat(dfd, "dstfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "renameat2: create dstfile");
    if (fd >= 0) {
        close(fd);
    }

    errno = 0;
    long r = syscall(SYS_renameat2, dfd, "srcfile", dfd, "dstfile", (unsigned int)RENAME_NOREPLACE);
    CHECK(r == -1 && errno == EEXIST, "renameat2(RENAME_NOREPLACE): dst exists -> EEXIST");

    unlinkat(dfd, "dstfile", 0);

    errno = 0;
    r = syscall(SYS_renameat2, dfd, "srcfile", dfd, "dstfile", (unsigned int)RENAME_NOREPLACE);
    CHECK(r == 0, "renameat2(RENAME_NOREPLACE): rename success when dst missing");

    struct stat st;
    errno = 0;
    CHECK(fstatat(dfd, "srcfile", &st, 0) == -1 && errno == ENOENT, "renameat2: srcfile missing after rename");
    CHECK_RET(fstatat(dfd, "dstfile", &st, 0), 0, "renameat2: dstfile exists after rename");

    errno = 0;
    r = syscall(SYS_renameat2, dfd, "dstfile", dfd, "dstfile2", 0x80000000u);
    CHECK(r == -1 && errno == EINVAL, "renameat2: invalid flags -> EINVAL");

    unlinkat(dfd, "dstfile", 0);
    close(dfd);
}

static void test_linkat_success_and_failures(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    int fd = openat(dfd, "origfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "linkat: create origfile");
    if (fd >= 0) {
        write(fd, "hardlink", 8);
        close(fd);
    }

    CHECK_RET(linkat(dfd, "origfile", dfd, "hardlink", 0), 0, "linkat: create hardlink");

    struct stat st1, st2;
    CHECK_RET(fstatat(dfd, "origfile", &st1, 0), 0, "linkat: stat origfile");
    CHECK_RET(fstatat(dfd, "hardlink", &st2, 0), 0, "linkat: stat hardlink");
    CHECK(st1.st_ino == st2.st_ino, "linkat: st_ino match");
    CHECK(st1.st_dev == st2.st_dev, "linkat: st_dev match");
    CHECK(st1.st_nlink == 2, "linkat: st_nlink == 2");

    mkdirat(dfd, "dirlink_src", 0755);
    CHECK_ERR(linkat(dfd, "dirlink_src", dfd, "dirlink_dst", 0), EPERM, "linkat: directory hardlink -> EPERM");
    unlinkat(dfd, "dirlink_dst", 0);
    unlinkat(dfd, "dirlink_src", AT_REMOVEDIR);

    unlinkat(dfd, "origfile", 0);
    unlinkat(dfd, "hardlink", 0);
    close(dfd);
}

static void test_symlinkat_and_readlinkat(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    int fd = openat(dfd, "realfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "symlinkat: create realfile");
    if (fd >= 0) {
        write(fd, "symlink_data", 12);
        close(fd);
    }

    CHECK_RET(symlinkat("realfile", dfd, "symlink"), 0, "symlinkat: create symlink");

    struct stat st;
    CHECK_RET(fstatat(dfd, "symlink", &st, 0), 0, "symlinkat: fstatat follow");
    CHECK(S_ISREG(st.st_mode), "symlinkat: follow sees regular file");
    CHECK_RET(fstatat(dfd, "symlink", &st, AT_SYMLINK_NOFOLLOW), 0, "symlinkat: fstatat nofollow");
    CHECK(S_ISLNK(st.st_mode), "symlinkat: nofollow sees symlink");

    char linkbuf[256];
    ssize_t linklen = readlinkat(dfd, "symlink", linkbuf, sizeof(linkbuf) - 1);
    CHECK(linklen > 0, "readlinkat: returns positive length");
    if (linklen > 0) {
        linkbuf[linklen] = '\0';
        CHECK(strcmp(linkbuf, "realfile") == 0, "readlinkat: target path equals realfile");
    }

    char small[4];
    linklen = readlinkat(dfd, "symlink", small, sizeof(small));
    CHECK_RET(linklen, (ssize_t)sizeof(small), "readlinkat: truncation returns buffer size");
    CHECK(memcmp(small, "real", sizeof(small)) == 0, "readlinkat: truncation keeps prefix");

    errno = 0;
    long r = syscall(SYS_readlinkat, dfd, "symlink", linkbuf, 0);
    CHECK_RET(r, 0, "readlinkat: size==0 -> 0");
    CHECK(errno == 0, "readlinkat: size==0 keeps errno==0");
    CHECK_ERR(readlinkat(dfd, "realfile", linkbuf, sizeof(linkbuf)), EINVAL, "readlinkat: non-symlink -> EINVAL");
    CHECK_ERR(readlinkat(dfd, "no_such_link", linkbuf, sizeof(linkbuf)), ENOENT, "readlinkat: missing -> ENOENT");

    unlinkat(dfd, "symlink", 0);
    unlinkat(dfd, "realfile", 0);
    close(dfd);
}

static void test_getdents64_success_and_einval(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    mkdirat(dfd, "testdir", 0755);
    int tfd = openat(dfd, "testdir/testfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (tfd >= 0) {
        close(tfd);
    }

    int dirfd = openat(dfd, "testdir", O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "getdents64: open testdir");
    if (dirfd < 0) {
        unlinkat(dfd, "testdir/testfile", 0);
        unlinkat(dfd, "testdir", AT_REMOVEDIR);
        close(dfd);
        return;
    }

    char small_buf[1];
    errno = 0;
    long nread = syscall(SYS_getdents64, dirfd, small_buf, sizeof(small_buf));
    CHECK(nread == -1 && errno == EINVAL, "getdents64: tiny buffer -> EINVAL");

    char buf[4096];
    errno = 0;
    nread = syscall(SYS_getdents64, dirfd, buf, sizeof(buf));
    CHECK(nread > 0, "getdents64: returns positive length");

    int found_self = 0;
    int found_parent = 0;
    int found_file = 0;
    long pos = 0;
    while (pos + (long)sizeof(struct linux_dirent64) <= nread) {
        struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + pos);
        if (d->d_reclen == 0) {
            break;
        }
        if (strcmp(d->d_name, ".") == 0) {
            found_self = 1;
        } else if (strcmp(d->d_name, "..") == 0) {
            found_parent = 1;
        } else if (strcmp(d->d_name, "testfile") == 0) {
            found_file = 1;
        }
        pos += d->d_reclen;
    }

    CHECK(found_self, "getdents64: found '.'");
    CHECK(found_parent, "getdents64: found '..'");
    CHECK(found_file, "getdents64: found 'testfile'");

    close(dirfd);
    unlinkat(dfd, "testdir/testfile", 0);
    unlinkat(dfd, "testdir", AT_REMOVEDIR);
    close(dfd);
}

static void test_fchdir_success_and_failures(void)
{
    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "fchdir: capture old cwd");

    char target[256];
    path_join(target, sizeof(target), "fchdir_target");
    mkdir(target, 0755);

    int fd = open(target, O_RDONLY | O_DIRECTORY);
    CHECK(fd >= 0, "fchdir: open target dir");
    if (fd >= 0) {
        CHECK_RET(fchdir(fd), 0, "fchdir: change to fd");
        char cwd[512];
        CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "fchdir: getcwd after fchdir");
        CHECK(strcmp(cwd, target) == 0, "fchdir: cwd equals target");
        chdir(old_cwd);
        close(fd);
    }

    rmdir(target);

    CHECK_ERR(fchdir(-1), EBADF, "fchdir: fd=-1 -> EBADF");

    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");
    int rfd = open(regfile, O_RDONLY);
    CHECK(rfd >= 0, "fchdir: open regfile");
    if (rfd >= 0) {
        CHECK_ERR(fchdir(rfd), ENOTDIR, "fchdir: regfile fd -> ENOTDIR");
        close(rfd);
    }
}

static void test_mknodat_success_and_failures(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    CHECK_RET(mknodat(dfd, "mknod_reg", S_IFREG | 0644, 0), 0, "mknodat: create regular file");
    struct stat st;
    CHECK_RET(fstatat(dfd, "mknod_reg", &st, 0), 0, "mknodat: fstatat mknod_reg");
    CHECK(S_ISREG(st.st_mode), "mknodat: mknod_reg is regular file");
    unlinkat(dfd, "mknod_reg", 0);

    CHECK_ERR(mknodat(dfd, "mknod_dir", S_IFDIR | 0755, 0), EPERM, "mknodat: S_IFDIR -> EPERM");
    close(dfd);
}

static void test_chroot_conditional_privilege(void)
{
    uid_t euid = geteuid();
    if (euid == 0) {
        CHECK_RET(chroot("/"), 0, "chroot: root chroot('/')");
    } else {
        CHECK_ERR(chroot("/"), EPERM, "chroot: non-root chroot('/') -> EPERM");
    }

    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");
    if (euid == 0) {
        CHECK_ERR(chroot(regfile), ENOTDIR, "chroot: regfile -> ENOTDIR");
    } else {
        CHECK_ERR(chroot(regfile), EPERM, "chroot: non-root regfile -> EPERM");
    }
}

int main(void)
{
    TEST_START("path-family: mkdirat/getcwd/chdir/unlinkat/rename/linkat/symlinkat/getdents64/fchdir/mknodat/chroot/readlinkat/renameat2");

    atexit(teardown);
    setup();

    test_mkdirat_success();
    test_mkdirat_eexist();
    test_mkdirat_ebadf();
    test_mkdirat_enotdir();
    test_mkdirat_enoent();

    test_getcwd_success_and_erange();
    test_chdir_success_and_failures();
    test_unlinkat_success_and_failures();
    test_renameat_cross_dir();
    test_renameat2_noreplace_and_invalid_flags();
    test_linkat_success_and_failures();
    test_symlinkat_and_readlinkat();
    test_getdents64_success_and_einval();
    test_fchdir_success_and_failures();
    test_mknodat_success_and_failures();
    test_chroot_conditional_privilege();

    TEST_DONE();
}
