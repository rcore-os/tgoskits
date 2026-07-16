#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <utime.h>
#include <unistd.h>

static char base[PATH_MAX];
static char regular_file[PATH_MAX];
static char denied_dir[PATH_MAX];

static void join_path(char *out, size_t size, const char *name)
{
    int ret = snprintf(out, size, "%s/%s", base, name);
    CHECK(ret > 0 && (size_t)ret < size, "build fixture path");
}

static int permission_child(void)
{
    pid_t pid = fork();
    if (pid < 0)
        return 0;
    if (pid == 0) {
        char denied_link[PATH_MAX];
        char denied_existing_link[PATH_MAX];
        struct utimbuf explicit_times = {.actime = 10, .modtime = 20};
        int ok = snprintf(denied_link, sizeof(denied_link), "%s/link", denied_dir) > 0;
        ok = ok && snprintf(denied_existing_link, sizeof(denied_existing_link),
                            "%s/existing-link", denied_dir) > 0;
        ok = ok && setresuid(1000, 1000, 1000) == 0;
        errno = 0;
        ok = ok && symlink("target", denied_existing_link) == -1 && errno == EEXIST;
        errno = 0;
        ok = ok && symlink("target", denied_link) == -1 && errno == EACCES;
        errno = 0;
        ok = ok && utime(regular_file, NULL) == -1 && errno == EACCES;
        errno = 0;
        ok = ok && utime(regular_file, &explicit_times) == -1 && errno == EPERM;
        _exit(ok ? 0 : 1);
    }
    int status = 0;
    return waitpid(pid, &status, 0) == pid && WIFEXITED(status) &&
           WEXITSTATUS(status) == 0;
}

static void cleanup(void)
{
    char path[PATH_MAX];
    const char *names[] = {
        "link", "missing-link", "loop-a", "loop-b", "component", NULL,
    };
    chmod(denied_dir, 0755);
    for (size_t i = 0; names[i] != NULL; i++) {
        join_path(path, sizeof(path), names[i]);
        unlink(path);
    }
    snprintf(path, sizeof(path), "%s/link", denied_dir);
    unlink(path);
    snprintf(path, sizeof(path), "%s/existing-link", denied_dir);
    unlink(path);
    unlink(regular_file);
    rmdir(denied_dir);
    rmdir(base);
}

int main(void)
{
    TEST_START("SyscallGuard final symlink and utime runtime behavior");

    snprintf(base, sizeof(base), "/tmp/syscallguard-final-fs-%ld", (long)getpid());
    join_path(regular_file, sizeof(regular_file), "file");
    join_path(denied_dir, sizeof(denied_dir), "denied");
    cleanup();
    CHECK_RET(mkdir(base, 0755), 0, "create fixture directory");
    CHECK_RET(mkdir(denied_dir, 0555), 0, "create non-writable directory");

    char denied_existing_link[PATH_MAX];
    int ret = snprintf(denied_existing_link, sizeof(denied_existing_link),
                       "%s/existing-link", denied_dir);
    CHECK(ret > 0 && (size_t)ret < sizeof(denied_existing_link),
          "build existing denied-link path");
    CHECK_RET(symlink("existing-target", denied_existing_link), 0,
              "create existing link in non-writable directory");

    int fd = open(regular_file, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0, "create utime fixture");
    if (fd >= 0)
        CHECK_RET(close(fd), 0, "close utime fixture");

    char link_path[PATH_MAX];
    join_path(link_path, sizeof(link_path), "link");
    CHECK_RET(symlink("file", link_path), 0, "symlink creates a relative target");
    CHECK_ERR(symlink("file", link_path), EEXIST, "symlink existing link returns EEXIST");

    char missing_parent[PATH_MAX];
    ret = snprintf(missing_parent, sizeof(missing_parent), "%s/missing/link", base);
    CHECK(ret > 0 && (size_t)ret < sizeof(missing_parent), "build missing-parent path");
    CHECK_ERR(symlink("file", missing_parent), ENOENT,
              "symlink missing parent returns ENOENT");

    char component[PATH_MAX];
    char not_directory[PATH_MAX];
    join_path(component, sizeof(component), "component");
    fd = open(component, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "create non-directory component");
    if (fd >= 0)
        close(fd);
    ret = snprintf(not_directory, sizeof(not_directory), "%s/child", component);
    CHECK(ret > 0 && (size_t)ret < sizeof(not_directory), "build ENOTDIR path");
    CHECK_ERR(symlink("file", not_directory), ENOTDIR,
              "symlink regular-file component returns ENOTDIR");

    char too_long[PATH_MAX + 32];
    memset(too_long, 'x', sizeof(too_long) - 1);
    too_long[sizeof(too_long) - 1] = '\0';
    CHECK_ERR(symlink("file", too_long), ENAMETOOLONG,
              "symlink overlong link path returns ENAMETOOLONG");

    struct utimbuf times = {.actime = 100, .modtime = 200};
    CHECK_RET(utime(regular_file, &times), 0, "utime sets explicit timestamps");
    struct stat st;
    CHECK_RET(stat(regular_file, &st), 0, "stat explicit utime fixture");
    CHECK(st.st_mtime == 200, "utime stores the requested mtime");
    CHECK_RET(utime(regular_file, NULL), 0, "utime NULL uses current time");

    times.actime = 300;
    times.modtime = 400;
    CHECK_RET(utime(link_path, &times), 0, "utime follows symlink target");
    CHECK_RET(stat(regular_file, &st), 0, "stat symlink target after utime");
    CHECK(st.st_mtime == 400, "utime through symlink updates target mtime");

    CHECK_ERR(utime("", NULL), ENOENT, "utime empty path returns ENOENT");
    char missing_file[PATH_MAX];
    join_path(missing_file, sizeof(missing_file), "missing-file");
    CHECK_ERR(utime(missing_file, &times), ENOENT,
              "utime missing file returns ENOENT");

    char loop_a[PATH_MAX];
    char loop_b[PATH_MAX];
    join_path(loop_a, sizeof(loop_a), "loop-a");
    join_path(loop_b, sizeof(loop_b), "loop-b");
    CHECK_RET(symlink("loop-b", loop_a), 0, "create first symlink loop edge");
    CHECK_RET(symlink("loop-a", loop_b), 0, "create second symlink loop edge");
    CHECK_ERR(utime(loop_a, &times), ELOOP, "utime symlink loop returns ELOOP");

    CHECK(permission_child(),
          "unprivileged symlink and utime return EACCES/EPERM");

    cleanup();
    TEST_DONE();
}
