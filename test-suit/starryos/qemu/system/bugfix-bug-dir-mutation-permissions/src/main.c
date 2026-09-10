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
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/mount.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE (1U << 0)
#endif
#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif
#ifndef SYS_openat2
#define SYS_openat2 437
#endif
#ifndef RESOLVE_NO_SYMLINKS
#define RESOLVE_NO_SYMLINKS 0x04
#endif
#ifndef RESOLVE_BENEATH
#define RESOLVE_BENEATH 0x08
#endif
#ifndef PR_SET_KEEPCAPS
#define PR_SET_KEEPCAPS 8
#endif

#define LINUX_CAPABILITY_VERSION_3 0x20080522U
#define CAP_DAC_READ_SEARCH 2

struct capability_header {
    uint32_t version;
    int32_t pid;
};

struct capability_data {
    uint32_t effective;
    uint32_t permitted;
    uint32_t inheritable;
};

struct open_how {
    uint64_t flags;
    uint64_t mode;
    uint64_t resolve;
};

static const char *const base = "/tmp/bug-dir-mutation-permissions";
static const char *const protected_dir = "/tmp/bug-dir-mutation-permissions/protected";
static const char *const sticky_dir = "/tmp/bug-dir-mutation-permissions/sticky";
static const char *const source = "/tmp/bug-dir-mutation-permissions/source";
static const char *const protected_file = "/tmp/bug-dir-mutation-permissions/protected/file";
static const char *const protected_dangling_middle =
    "/tmp/bug-dir-mutation-permissions/protected/dangling-middle";
static const char *const protected_dangling_path =
    "/tmp/bug-dir-mutation-permissions/protected/dangling-middle/leaf";
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
static const char *const empty_path_source =
    "/tmp/bug-dir-mutation-permissions/empty-path-source";
static const char *const empty_path_link =
    "/tmp/bug-dir-mutation-permissions/public/empty-path-link";
static const char *const dirfd_existing =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/existing";
static const char *const dirfd_created_by_openat =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/created-by-openat";
static const char *const dirfd_created_by_mkdirat =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/created-by-mkdirat";
static const char *const dirfd_created_by_linkat =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/created-by-linkat";
static const char *const dirfd_rename_source =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/rename-source";
static const char *const dirfd_rename_target =
    "/tmp/bug-dir-mutation-permissions/dirfd-parent/opened/rename-target";
static const char *const mount_a = "/tmp/bug-dir-mutation-permissions/mount-a";
static const char *const mount_b = "/tmp/bug-dir-mutation-permissions/mount-b";
static int dirfd = -1;
static int empty_path_source_fd = -1;

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

static long symlinkat_call(const char *target, const char *linkpath)
{
    return syscall(SYS_symlinkat, target, AT_FDCWD, linkpath);
}

static void remove_if_present(const char *path)
{
    unlink(path);
    rmdir(path);
}

static void cleanup_dirfd_tree(void)
{
    remove_if_present(dirfd_created_by_openat);
    remove_if_present(dirfd_created_by_mkdirat);
    remove_if_present(dirfd_created_by_linkat);
    remove_if_present(dirfd_rename_source);
    remove_if_present(dirfd_rename_target);
    remove_if_present(dirfd_existing);
    remove_if_present(dirfd_root);
    remove_if_present(dirfd_parent);
}

static int run_unprivileged_checks(void)
{
    if (setuid(1000) < 0) {
        perror("setuid");
        return 1;
    }

    errno = 0;
    check(mknodat(AT_FDCWD, "", S_IFREG | 0600, 0) < 0 && errno == ENOENT,
          "mknodat rejects an empty pathname");

    errno = 0;
    check(symlinkat_call("target", "") < 0 && errno == ENOENT,
          "symlinkat rejects an empty pathname");

    errno = 0;
    check(linkat(AT_FDCWD, source, AT_FDCWD, "", 0) < 0 && errno == ENOENT,
          "linkat rejects an empty destination pathname");

    errno = 0;
    check(renameat2_call("", source, 0) < 0 && errno == ENOENT,
          "renameat2 rejects an empty source pathname");

    errno = 0;
    check(renameat2_call(source, "", 0) < 0 && errno == ENOENT,
          "renameat2 rejects an empty destination pathname");

    errno = 0;
    int inaccessible = open(protected_file, O_RDONLY | O_CREAT, 0600);
    check(inaccessible < 0 && errno == EACCES,
          "open O_CREAT checks an inaccessible parent before opening an existing target");
    if (inaccessible >= 0) {
        close(inaccessible);
    }

    errno = 0;
    check(unlink("/tmp/bug-dir-mutation-permissions/protected/missing") < 0
              && errno == EACCES,
          "unlink checks an unsearchable parent before a missing final entry");

    errno = 0;
    check(rmdir("/tmp/bug-dir-mutation-permissions/protected/missing-dir") < 0
              && errno == EACCES,
          "rmdir checks an unsearchable parent before a missing final entry");

    errno = 0;
    int dangling_middle = open(protected_dangling_path, O_RDONLY);
    check(dangling_middle < 0 && errno == EACCES,
          "an unsearchable parent beats ENOENT from a dangling intermediate symlink");
    if (dangling_middle >= 0) {
        close(dangling_middle);
    }

    errno = 0;
    const struct open_how openat2_how = {
        .flags = O_RDONLY,
        .mode = 0,
        .resolve = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS,
    };
    int openat2_root = open(base, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    int openat2_missing = openat2_root < 0
                              ? -1
                              : (int)syscall(SYS_openat2, openat2_root,
                                             "protected/openat2-missing",
                                             &openat2_how, sizeof(openat2_how));
    check(openat2_missing < 0 && errno == EACCES,
          "openat2 checks an unsearchable parent before a missing final entry");
    if (openat2_missing >= 0) {
        close(openat2_missing);
    }
    if (openat2_root >= 0) {
        close(openat2_root);
    }

    errno = 0;
    check(mkdir(protected_file, 0700) < 0 && errno == EACCES,
          "mkdir checks an inaccessible parent before an existing target");

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
    check(linkat(AT_FDCWD, source, AT_FDCWD, protected_link_new, 0) < 0
              && errno == EACCES,
          "linkat checks an inaccessible parent before a missing symlink target");

    errno = 0;
    check(renameat2_call(source, protected_link_new, 0) < 0 && errno == EACCES,
          "renameat2 checks an inaccessible parent before a missing symlink target");

    errno = 0;
    check(linkat(AT_FDCWD, "/tmp/bug-dir-mutation-permissions/protected/missing-source",
                 AT_FDCWD, "/tmp/bug-dir-mutation-permissions/public/missing-link", 0)
                  < 0
              && errno == EACCES,
          "linkat checks the source parent before a missing final entry");

    errno = 0;
    check(renameat2_call("/tmp/bug-dir-mutation-permissions/protected/missing-source",
                         "/tmp/bug-dir-mutation-permissions/public/missing-rename", 0)
              < 0
              && errno == EACCES,
          "renameat2 checks the source parent before a missing final entry");

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

    if (empty_path_source_fd >= 0) {
        errno = 0;
        check(linkat(empty_path_source_fd, "", AT_FDCWD, empty_path_link, AT_EMPTY_PATH) < 0
                  && errno == ENOENT,
              "linkat AT_EMPTY_PATH requires CAP_DAC_READ_SEARCH");
        check(access(empty_path_link, F_OK) < 0 && errno == ENOENT,
              "failed AT_EMPTY_PATH link leaves the destination unchanged");
    }

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
    check(symlink("target", sticky_file) < 0 && errno == EEXIST,
          "symlink reports an existing target in a searchable parent");

    errno = 0;
    check(symlink("target", protected_file) < 0 && errno == EACCES,
          "symlink checks an inaccessible parent before an existing target");

    errno = 0;
    check(mknodat(AT_FDCWD, sticky_file, S_IFREG | 0600, 0) < 0 && errno == EEXIST,
          "mknodat reports an existing target in a searchable parent");

    errno = 0;
    check(mknodat(AT_FDCWD, protected_file, S_IFREG | 0600, 0) < 0 && errno == EACCES,
          "mknodat checks an inaccessible parent before an existing target");

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

static int run_dac_read_search_checks(void)
{
    struct capability_header header = {
        .version = LINUX_CAPABILITY_VERSION_3,
        .pid = 0,
    };
    struct capability_data data[2] = {0};

    if (syscall(SYS_prctl, PR_SET_KEEPCAPS, 1, 0, 0, 0) != 0) {
        perror("PR_SET_KEEPCAPS");
        return 1;
    }
    if (setuid(1000) != 0) {
        perror("setuid");
        return 1;
    }

    data[0].effective = 1U << CAP_DAC_READ_SEARCH;
    data[0].permitted = 1U << CAP_DAC_READ_SEARCH;
    if (syscall(SYS_capset, &header, data) != 0) {
        perror("capset(CAP_DAC_READ_SEARCH)");
        return 1;
    }

    errno = 0;
    int fd = open(protected_file, O_RDONLY);
    check(fd >= 0,
          "CAP_DAC_READ_SEARCH permits reading through an unsearchable parent");
    if (fd >= 0) {
        close(fd);
    }

    errno = 0;
    check(mkdir(protected_new_file, 0700) < 0 && errno == EACCES,
          "CAP_DAC_READ_SEARCH does not grant parent write permission");

    errno = 0;
    check(mkdir(protected_file, 0700) < 0 && errno == EEXIST,
          "CAP_DAC_READ_SEARCH keeps existing-target precedence");

    errno = 0;
    fd = open(dangling_link, O_WRONLY | O_CREAT, 0600);
    check(fd < 0 && errno == EACCES,
          "CAP_DAC_READ_SEARCH cannot create through a dangling link without write access");
    if (fd >= 0) {
        close(fd);
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
    cleanup_dirfd_tree();
    remove_if_present(base);
    check(mkdir(base, 0755) == 0, "create test root");
    check(mkdir(protected_dir, 0700) == 0, "create protected directory");
    check(mkdir(public_dir, 0777) == 0, "create public directory");
    check(chmod(public_dir, 0777) == 0, "make public directory writable");
    check(create_file(source) == 0, "create hard-link source");
    check(create_file(empty_path_source) == 0, "create AT_EMPTY_PATH source");
    check(chown(empty_path_source, 1000, 1000) == 0,
          "assign AT_EMPTY_PATH source to the unprivileged user");
    empty_path_source_fd = open(empty_path_source, O_RDONLY);
    check(empty_path_source_fd >= 0, "open AT_EMPTY_PATH source before dropping privileges");
    check(create_file(protected_file) == 0, "create protected victim");
    check(chmod(protected_file, 0644) == 0,
          "make protected victim readable after setup");
    check(symlink(public_dir, protected_link) == 0,
          "create symlink through protected directory");
    check(symlink("missing-target", protected_dangling_middle) == 0,
          "create dangling intermediate symlink in protected directory");
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

    child = fork();
    check(child >= 0, "fork CAP_DAC_READ_SEARCH test process");
    if (child == 0) {
        int result = run_dac_read_search_checks();
        fflush(stdout);
        _exit(result);
    }
    status = 0;
    waited = waitpid(child, &status, 0);
    check(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "CAP_DAC_READ_SEARCH bypasses only read/search checks");
    if (waited == child && WIFSIGNALED(status)) {
        printf("FAIL: CAP_DAC_READ_SEARCH child terminated by signal %d\n",
               WTERMSIG(status));
    }

    remove_if_present("/tmp/bug-dir-mutation-permissions/sticky/new-dir");
    remove_if_present("/tmp/bug-dir-mutation-permissions/sticky/root-dir");
    remove_if_present(sticky_target);
    remove_if_present(sticky_file);
    remove_if_present(sticky_dir);
    remove_if_present(dangling_link);
    remove_if_present(protected_dangling_middle);
    remove_if_present(protected_new_file);
    remove_if_present(protected_file);
    remove_if_present(public_link_new);
    remove_if_present(protected_link);
    remove_if_present(protected_dir);
    remove_if_present(empty_path_link);
    remove_if_present(public_dir);
    remove_if_present(source);
    if (empty_path_source_fd >= 0) {
        close(empty_path_source_fd);
    }
    remove_if_present(empty_path_source);
    if (dirfd >= 0) {
        close(dirfd);
    }
    cleanup_dirfd_tree();
    remove_if_present(base);

    printf("DIR_MUTATION_PERMISSIONS_TEST_%s\n", failures == 0 ? "PASSED" : "FAILED");
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
