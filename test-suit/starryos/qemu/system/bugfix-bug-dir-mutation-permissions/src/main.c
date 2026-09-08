/*
 * Regression coverage for directory mutation authorization.
 *
 * Linux requires write+search permission on mutation parents and applies the
 * sticky-directory owner rules to unlink/rmdir/rename.  The test runs the
 * operations from an unprivileged child so a successful mutation cannot be
 * mistaken for the root process's capability bypass.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/mount.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1U << 0)
#endif

static const char *const base = "/tmp/bug-dir-mutation-permissions";
static const char *const protected_dir = "/tmp/bug-dir-mutation-permissions/protected";
static const char *const sticky_dir = "/tmp/bug-dir-mutation-permissions/sticky";
static const char *const source = "/tmp/bug-dir-mutation-permissions/source";
static const char *const protected_file = "/tmp/bug-dir-mutation-permissions/protected/file";
static const char *const protected_new_file =
    "/tmp/bug-dir-mutation-permissions/protected/created-by-symlink";
static const char *const public_dir = "/tmp/bug-dir-mutation-permissions/public";
static const char *const protected_link =
    "/tmp/bug-dir-mutation-permissions/protected/public-link";
static const char *const public_link_new =
    "/tmp/bug-dir-mutation-permissions/public/new-through-link";
static const char *const protected_link_new =
    "/tmp/bug-dir-mutation-permissions/protected/public-link/new";
static const char *const dangling_link =
    "/tmp/bug-dir-mutation-permissions/dangling-link";
static const char *const sticky_file = "/tmp/bug-dir-mutation-permissions/sticky/root-file";
static const char *const sticky_target = "/tmp/bug-dir-mutation-permissions/sticky/root-target";
static const char *const sticky_child = "/tmp/bug-dir-mutation-permissions/sticky/child-file";
static const char *const dirfd_parent = "/tmp/bug-dir-mutation-permissions/dirfd-parent";
static const char *const dirfd_root = "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened";
static const char *const dirfd_existing =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/existing";
static const char *const mount_a = "/tmp/bug-dir-mutation-permissions/mount-a";
static const char *const mount_b = "/tmp/bug-dir-mutation-permissions/mount-b";
static int dirfd = -1;

static int failures;

static void check(int condition, const char *message)
{
    if (condition) {
        printf("PASS: %s\n", message);
    } else {
        printf("FAIL: %s (errno=%d)\n", message, errno);
        failures++;
    }
}

static int create_file(const char *path)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return -1;
    }
    close(fd);
    return 0;
}

static long renameat2_call(const char *old_path, const char *new_path,
                           unsigned int flags)
{
    return syscall(SYS_renameat2, AT_FDCWD, old_path, AT_FDCWD, new_path, flags);
}

static void remove_if_present(const char *path)
{
    unlink(path);
    rmdir(path);
}

static int run_unprivileged_checks(void)
{
    if (setuid(1000) < 0) {
        perror("setuid");
        return 1;
    }

    errno = 0;
    check(mkdirat(AT_FDCWD, "/tmp/bug-dir-mutation-permissions/protected/new", 0700) < 0
              && errno == EACCES,
          "mkdirat rejects an unwritable parent");

    errno = 0;
    check(linkat(AT_FDCWD, source, AT_FDCWD,
                 "/tmp/bug-dir-mutation-permissions/protected/link", 0) < 0
              && errno == EACCES,
          "linkat rejects an unwritable parent");

    errno = 0;
    check(unlinkat(AT_FDCWD, protected_file, 0) < 0 && errno == EACCES,
          "unlinkat rejects an unsearchable parent");

    errno = 0;
    check(renameat2_call(protected_file,
                         "/tmp/bug-dir-mutation-permissions/protected/renamed", 0)
              < 0
              && errno == EACCES,
          "renameat2 rejects an unsearchable parent");

    errno = 0;
    check(mkdir("/tmp/bug-dir-mutation-permissions/protected", 0700) < 0
              && errno == EEXIST,
          "mkdir reports an existing target before checking parent write access");

    errno = 0;
    check(unlink(sticky_file) < 0 && errno == EPERM,
          "sticky directory protects a file owned by another user");
    check(access(sticky_file, F_OK) == 0, "sticky unlink leaves the victim unchanged");

    errno = 0;
    check(rename(sticky_file, sticky_file) == 0,
          "ordinary rename of the same sticky entry is a no-op");

    errno = 0;
    check(rename(sticky_file, "/tmp/bug-dir-mutation-permissions/sticky/renamed") < 0
              && errno == EPERM,
          "sticky directory protects rename of another user's file");
    check(access(sticky_file, F_OK) == 0, "sticky rename leaves the source unchanged");

    errno = 0;
    int fd = open(dangling_link, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    check(fd < 0 && errno == EACCES,
          "openat O_CREAT rejects a symlink target in an unwritable directory");
    if (fd >= 0) {
        close(fd);
    }

    errno = 0;
    int through_link = syscall(SYS_openat, AT_FDCWD, protected_link_new,
                               O_WRONLY | O_CREAT | O_EXCL, 0600);
    check(through_link < 0 && errno == EACCES,
          "openat checks an inaccessible directory before following a symlink");
    if (through_link >= 0) {
        close(through_link);
        unlink(public_link_new);
    }
    errno = 0;
    check(access(public_link_new, F_OK) < 0 && errno == ENOENT,
          "symlink permission failure leaves the resolved target unchanged");

    errno = 0;
    check(rmdir("/tmp/bug-dir-mutation-permissions/sticky/root-dir") < 0
              && errno == EPERM,
          "sticky directory protects another user's directory");

    errno = 0;
    check(mkdir("/tmp/bug-dir-mutation-permissions/sticky/new-dir", 0700) == 0,
          "sticky directory still permits creation with write+search access");

    errno = 0;
    check(linkat(AT_FDCWD, source, AT_FDCWD,
                 "/tmp/bug-dir-mutation-permissions/sticky/root-link", 0) < 0
              && errno == EPERM,
          "linkat rejects a hard link to another user's file");

    check(create_file(sticky_child) == 0, "unprivileged user creates its own file");
    errno = 0;
    check(renameat2_call(sticky_child, sticky_target, RENAME_NOREPLACE) < 0
              && errno == EEXIST,
          "RENAME_NOREPLACE reports an existing target before sticky checks");
    check(access(sticky_child, F_OK) == 0 && access(sticky_target, F_OK) == 0,
          "failed RENAME_NOREPLACE leaves both entries unchanged");

    errno = 0;
    check(rename(sticky_child, sticky_target) < 0 && errno == EPERM,
          "sticky rename protects an existing target owned by another user");
    check(access(sticky_child, F_OK) == 0 && access(sticky_target, F_OK) == 0,
          "failed sticky replacement leaves both entries unchanged");
    check(unlink(sticky_child) == 0, "file owner can unlink its own sticky entry");

    errno = 0;
    check(symlink("target", protected_file) < 0 && errno == EEXIST,
          "symlink reports an existing target before parent permissions");

    errno = 0;
    check(mknodat(AT_FDCWD, protected_file, S_IFREG | 0600, 0) < 0 && errno == EEXIST,
          "mknodat reports an existing target before parent permissions");

    if (dirfd >= 0) {
        int opened = openat(dirfd, "existing", O_RDONLY);
        check(opened >= 0, "openat uses the opened dirfd as permission boundary");
        if (opened >= 0) {
            close(opened);
        }

        opened = openat(dirfd, "created-by-openat", O_WRONLY | O_CREAT | O_EXCL, 0600);
        check(opened >= 0, "openat O_CREAT does not recheck the dirfd parent");
        if (opened >= 0) {
            close(opened);
        }

        check(mkdirat(dirfd, "created-by-mkdirat", 0700) == 0,
              "mkdirat does not recheck the dirfd parent");
        check(linkat(dirfd, "existing", dirfd, "created-by-linkat", 0) == 0,
              "linkat does not recheck the dirfd parent");
        check(unlinkat(dirfd, "created-by-linkat", 0) == 0,
              "unlinkat does not recheck the dirfd parent");

        opened = openat(dirfd, "rename-source", O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (opened >= 0) {
            close(opened);
        }
        errno = 0;
        check(syscall(SYS_renameat2, dirfd, "rename-source", dirfd, "rename-target", 0) == 0,
              "renameat2 does not recheck the dirfd parent");
    }

    return failures == 0 ? 0 : 1;
}

static void test_cross_mount_same_inode_rename(void)
{
    int fd_a = -1;
    int fd_b = -1;
    int mounted_a = 0;
    int mounted_b = 0;

    if (mkdir(mount_a, 0755) < 0 || mkdir(mount_b, 0755) < 0) {
        check(0, "mount independent filesystems for cross-device rename");
        goto cleanup;
    }
    if (mount("tmpfs", mount_a, "tmpfs", 0, NULL) < 0) {
        check(0, "mount independent filesystems for cross-device rename");
        goto cleanup;
    }
    mounted_a = 1;
    if (mount("tmpfs", mount_b, "tmpfs", 0, NULL) < 0) {
        check(0, "mount independent filesystems for cross-device rename");
        goto cleanup;
    }
    mounted_b = 1;

    fd_a = open(mount_a, O_RDONLY | O_DIRECTORY);
    fd_b = open(mount_b, O_RDONLY | O_DIRECTORY);
    check(fd_a >= 0 && fd_b >= 0, "open independent mount roots");
    if (fd_a >= 0 && fd_b >= 0) {
        int file_a = openat(fd_a, "same-inode", O_WRONLY | O_CREAT | O_EXCL, 0600);
        int file_b = openat(fd_b, "same-inode", O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (file_a >= 0) {
            close(file_a);
        }
        if (file_b >= 0) {
            close(file_b);
        }

        struct stat stat_a;
        struct stat stat_b;
        check(fstatat(fd_a, "same-inode", &stat_a, 0) == 0
                  && fstatat(fd_b, "same-inode", &stat_b, 0) == 0
                  && stat_a.st_ino == stat_b.st_ino,
              "independent mounts expose equal inode numbers for the regression");
        errno = 0;
        check(renameat(fd_a, "same-inode", fd_b, "same-inode") < 0 && errno == EXDEV,
              "rename across mounts returns EXDEV before inode no-op shortcut");
        check(fstatat(fd_a, "same-inode", &stat_a, 0) == 0
                  && fstatat(fd_b, "same-inode", &stat_b, 0) == 0,
              "cross-mount rename leaves both entries unchanged");
    }
cleanup:
    if (fd_a >= 0) {
        close(fd_a);
    }
    if (fd_b >= 0) {
        close(fd_b);
    }
    if (mounted_b) {
        umount(mount_b);
    }
    if (mounted_a) {
        umount(mount_a);
    }
    rmdir(mount_b);
    rmdir(mount_a);
}

int main(void)
{
    remove_if_present(base);
    check(mkdir(base, 0755) == 0, "create test root");
    check(mkdir(protected_dir, 0700) == 0, "create protected directory");
    check(mkdir(public_dir, 0777) == 0, "create public directory");
    check(chmod(public_dir, 0777) == 0, "make public directory writable");
    check(create_file(source) == 0, "create hard-link source");
    check(create_file(protected_file) == 0, "create protected victim");
    check(symlink(public_dir, protected_link) == 0,
          "create symlink through protected directory");
    check(symlink(protected_new_file, dangling_link) == 0, "create dangling symlink");
    check(mkdir(sticky_dir, 01777) == 0, "create sticky directory");
    check(chmod(sticky_dir, 01777) == 0, "restore sticky directory permissions");
    struct stat sticky_metadata;
    check(stat(sticky_dir, &sticky_metadata) == 0
              && (sticky_metadata.st_mode & 07777) == 01777,
          "sticky directory keeps mode 01777 after setup");
    check(create_file(sticky_file) == 0, "create sticky victim");
    check(create_file(sticky_target) == 0, "create sticky replacement target");
    check(mkdir("/tmp/bug-dir-mutation-permissions/sticky/root-dir", 0700) == 0,
          "create sticky directory victim");

    check(mkdir(dirfd_parent, 0755) == 0, "create dirfd parent");
    check(mkdir(dirfd_root, 0700) == 0, "create dirfd target");
    check(chown(dirfd_root, 1000, 1000) == 0, "assign dirfd target to unprivileged user");
    check(create_file(dirfd_existing) == 0, "create dirfd existing entry");
    check(chown(dirfd_existing, 1000, 1000) == 0,
          "assign dirfd source entry to unprivileged user");
    check(chmod(dirfd_existing, 0644) == 0, "make dirfd existing entry readable");
    dirfd = open(dirfd_root, O_RDONLY | O_DIRECTORY);
    check(dirfd >= 0, "open dirfd before restricting its parent");
    check(chmod(dirfd_parent, 0700) == 0, "remove search permission from dirfd parent");

    test_cross_mount_same_inode_rename();

    pid_t child = fork();
    check(child >= 0, "fork unprivileged test process");
    if (child == 0) {
        int result = run_unprivileged_checks();
        fflush(stdout);
        _exit(result);
    }
    int status = 0;
    int waited = waitpid(child, &status, 0);
    check(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "all unprivileged mutations are rejected or authorized");
    if (waited == child && WIFSIGNALED(status)) {
        printf("FAIL: unprivileged child terminated by signal %d\n", WTERMSIG(status));
    }

    remove_if_present("/tmp/bug-dir-mutation-permissions/sticky/new-dir");
    remove_if_present("/tmp/bug-dir-mutation-permissions/sticky/root-dir");
    remove_if_present(sticky_target);
    remove_if_present(sticky_file);
    remove_if_present(sticky_dir);
    remove_if_present(dangling_link);
    remove_if_present(protected_new_file);
    remove_if_present(protected_file);
    remove_if_present(public_link_new);
    remove_if_present(protected_link);
    remove_if_present(protected_dir);
    remove_if_present(public_dir);
    remove_if_present(source);
    if (dirfd >= 0) {
        close(dirfd);
    }
    remove_if_present(dirfd_existing);
    remove_if_present(dirfd_root);
    remove_if_present(dirfd_parent);
    remove_if_present(base);

    printf("DIR_MUTATION_PERMISSIONS_TEST_%s\n", failures == 0 ? "PASSED" : "FAILED");
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
