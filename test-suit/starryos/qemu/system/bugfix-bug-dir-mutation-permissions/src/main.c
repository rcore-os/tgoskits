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
static const char *const dangling_link =
    "/tmp/bug-dir-mutation-permissions/dangling-link";
static const char *const sticky_file = "/tmp/bug-dir-mutation-permissions/sticky/root-file";
static const char *const sticky_target = "/tmp/bug-dir-mutation-permissions/sticky/root-target";
static const char *const sticky_child = "/tmp/bug-dir-mutation-permissions/sticky/child-file";

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
    return failures == 0 ? 0 : 1;
}

int main(void)
{
    remove_if_present(base);
    check(mkdir(base, 0755) == 0, "create test root");
    check(mkdir(protected_dir, 0700) == 0, "create protected directory");
    check(create_file(source) == 0, "create hard-link source");
    check(create_file(protected_file) == 0, "create protected victim");
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

    pid_t child = fork();
    check(child >= 0, "fork unprivileged test process");
    if (child == 0) {
        _exit(run_unprivileged_checks());
    }
    int status = 0;
    check(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "all unprivileged mutations are rejected or authorized");

    remove_if_present("/tmp/bug-dir-mutation-permissions/sticky/new-dir");
    remove_if_present("/tmp/bug-dir-mutation-permissions/sticky/root-dir");
    remove_if_present(sticky_target);
    remove_if_present(sticky_file);
    remove_if_present(sticky_dir);
    remove_if_present(dangling_link);
    remove_if_present(protected_new_file);
    remove_if_present(protected_file);
    remove_if_present(protected_dir);
    remove_if_present(source);
    remove_if_present(base);

    printf("DIR_MUTATION_PERMISSIONS_TEST_%s\n", failures == 0 ? "PASSED" : "FAILED");
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
