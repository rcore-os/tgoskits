#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while (0)

#define TEST_START(name)                                                \
    printf("================================================\n");       \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

#define TEST_DONE()                                                     \
    printf("------------------------------------------------\n");       \
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);               \
    printf("================================================\n\n");     \
    return __fail > 0 ? 1 : 0

#define CGROUP2_PATH "/tmp/cg"
#define CGROUP_V1_PATH "/tmp/cg-v1"
#define CLONE_INTO_CGROUP (1ULL << 33)

struct clone_args {
    unsigned long long flags;
    unsigned long long pidfd;
    unsigned long long child_tid;
    unsigned long long parent_tid;
    unsigned long long exit_signal;
    unsigned long long stack;
    unsigned long long stack_size;
    unsigned long long tls;
    unsigned long long set_tid;
    unsigned long long set_tid_size;
    unsigned long long cgroup;
};

static void check_mkdir(const char *path, const char *msg)
{
    errno = 0;
    int ret = mkdir(path, 0755);
    int saved_errno = errno;
    CHECK(ret == 0 || saved_errno == EEXIST, msg);
}

static void expect_mkdir_ok(const char *path, const char *msg)
{
    errno = 0;
    int ret = mkdir(path, 0755);
    CHECK(ret == 0, msg);
}

static void expect_mkdir_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int ret = mkdir(path, 0755);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_rmdir_ok(const char *path, const char *msg)
{
    errno = 0;
    int ret = rmdir(path);
    CHECK(ret == 0, msg);
}

static void expect_rmdir_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int ret = rmdir(path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_open_create_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int fd = open(path, O_CREAT | O_WRONLY, 0644);
    int saved_errno = errno;
    if (fd >= 0) {
        close(fd);
    }
    errno = saved_errno;
    CHECK(fd == -1 && saved_errno == expected_errno, msg);
}

static void expect_open_dir_errno(const char *path, int expected_errno, const char *msg)
{
    errno = 0;
    int fd = open(path, O_RDONLY | O_DIRECTORY);
    int saved_errno = errno;
    if (fd >= 0) {
        close(fd);
    }
    errno = saved_errno;
    CHECK(fd == -1 && saved_errno == expected_errno, msg);
}

static void expect_path_exists(const char *path, const char *msg)
{
    struct stat st;
    errno = 0;
    int ret = stat(path, &st);
    CHECK(ret == 0, msg);
}

static void expect_path_missing(const char *path, const char *msg)
{
    struct stat st;
    errno = 0;
    int ret = stat(path, &st);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == ENOENT, msg);
}

static ssize_t read_text_file(const char *path, char *buf, size_t cap)
{
    if (cap == 0) {
        errno = EINVAL;
        return -1;
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }

    errno = 0;
    ssize_t nread = read(fd, buf, cap - 1);
    int saved_errno = errno;
    if (nread >= 0) {
        buf[nread] = '\0';
    }

    close(fd);
    errno = saved_errno;
    return nread;
}

static void expect_empty_file(const char *path, const char *msg)
{
    char buf[16];
    ssize_t nread = read_text_file(path, buf, sizeof(buf));
    CHECK(nread == 0, msg);
}

static int buffer_contains_pid(const char *buf, pid_t pid)
{
    const char *cursor = buf;

    while (*cursor != '\0') {
        char *end = NULL;
        errno = 0;
        long value = strtol(cursor, &end, 10);
        if (cursor != end && errno == 0 && value == (long)pid) {
            return 1;
        }

        while (*cursor != '\0' && *cursor != '\n') {
            cursor++;
        }
        while (*cursor == '\n' || *cursor == '\r') {
            cursor++;
        }
    }

    return 0;
}

static void expect_write_errno(const char *path, const char *data,
                               int expected_errno, const char *msg)
{
    int fd = open(path, O_WRONLY);
    if (fd < 0) {
        CHECK(0, msg);
        return;
    }

    errno = 0;
    ssize_t written = write(fd, data, strlen(data));
    int saved_errno = errno;
    close(fd);
    errno = saved_errno;
    CHECK(written == -1 && saved_errno == expected_errno, msg);
}

static void expect_link_errno(const char *old_path, const char *new_path,
                              int expected_errno, const char *msg)
{
    errno = 0;
    int ret = link(old_path, new_path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_symlink_errno(const char *target, const char *link_path,
                                 int expected_errno, const char *msg)
{
    errno = 0;
    int ret = symlink(target, link_path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_rename_errno(const char *old_path, const char *new_path,
                                int expected_errno, const char *msg)
{
    errno = 0;
    int ret = rename(old_path, new_path);
    int saved_errno = errno;
    CHECK(ret == -1 && saved_errno == expected_errno, msg);
}

static void expect_chdir_ok(const char *path, const char *msg)
{
    errno = 0;
    int ret = chdir(path);
    CHECK(ret == 0, msg);
}

static void expect_clone_into_cgroup(const char *path, const char *msg)
{
    int cgroup_fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (cgroup_fd < 0) {
        CHECK(0, msg);
        return;
    }

    struct clone_args args = {
        .flags = CLONE_INTO_CGROUP,
        .exit_signal = SIGCHLD,
        .cgroup = (unsigned long long)cgroup_fd,
    };
    errno = 0;
    pid_t pid = (pid_t)syscall(SYS_clone3, &args, sizeof(args));
    if (pid == 0) {
        char buf[128];
        ssize_t nread = read_text_file(CGROUP2_PATH "/into/cgroup.procs",
                                       buf, sizeof(buf));
        _exit(nread >= 0 && buffer_contains_pid(buf, getpid()) ? 0 : 1);
    }

    int saved_errno = errno;
    int status = 0;
    int waited = pid > 0 ? waitpid(pid, &status, 0) : -1;
    close(cgroup_fd);
    errno = saved_errno;
    CHECK(pid > 0 && waited == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          msg);
}

static void test_unlink_control_file_order(void)
{
    const char *group_path = CGROUP2_PATH "/unlink-order";
    const char *control_name = "cgroup.procs";
    struct stat st;
    int dirfd = -1;
    int group_created = 0;

    if (geteuid() != 0) {
        errno = 0;
        CHECK(0, "unlink order test requires privileged root actor");
        return;
    }

    errno = 0;
    int ret = mkdir(group_path, 0755);
    if (ret == 0) {
        group_created = 1;
    }
    CHECK(ret == 0, "create isolated cgroup for unlink order test");
    if (ret != 0) {
        return;
    }

    errno = 0;
    ret = fstatat(AT_FDCWD, CGROUP2_PATH "/unlink-order/cgroup.procs",
                  &st, 0);
    CHECK(ret == 0 && S_ISREG(st.st_mode),
          "cgroup.procs exists as a regular control file");
    if (ret != 0 || !S_ISREG(st.st_mode)) {
        goto cleanup;
    }

    errno = 0;
    ret = chmod(group_path, 0555);
    CHECK(ret == 0, "set isolated cgroup parent permissions to 0555");
    if (ret != 0) {
        goto cleanup;
    }
    errno = 0;
    ret = stat(group_path, &st);
    CHECK(ret == 0 && (st.st_mode & 0777) == 0555,
          "verify isolated cgroup parent permissions are 0555");
    if (ret != 0 || (st.st_mode & 0777) != 0555) {
        goto cleanup;
    }

    errno = 0;
    dirfd = open(group_path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    CHECK(dirfd >= 0, "open isolated cgroup directory");
    if (dirfd < 0) {
        goto cleanup;
    }

    errno = 0;
    long unlink_ret = syscall(SYS_unlinkat, dirfd, control_name, AT_REMOVEDIR);
    int saved_errno = errno;
    CHECK(unlink_ret == -1 && saved_errno == ENOTDIR,
          "raw unlinkat AT_REMOVEDIR on cgroup.procs fails with ENOTDIR");

    errno = 0;
    unlink_ret = syscall(SYS_unlinkat, dirfd, control_name, 0);
    saved_errno = errno;
    CHECK(unlink_ret == -1 && saved_errno == EPERM,
          "raw unlinkat on cgroup.procs fails with EPERM");

    pid_t child = fork();
    CHECK(child >= 0, "fork nonprivileged unlink order actor");
    if (child == 0) {
        uid_t real_uid, effective_uid, saved_uid;
        gid_t real_gid, effective_gid, saved_gid;
        int child_errno;
        long child_ret;

        if (setgroups(0, NULL) != 0) {
            _exit(10);
        }
        if (setresgid(1000, 1000, 1000) != 0) {
            _exit(11);
        }
        if (setresuid(1000, 1000, 1000) != 0) {
            _exit(12);
        }
        if (getresgid(&real_gid, &effective_gid, &saved_gid) != 0 ||
            getresuid(&real_uid, &effective_uid, &saved_uid) != 0 ||
            real_uid != 1000 || effective_uid != 1000 || saved_uid != 1000 ||
            real_gid != 1000 || effective_gid != 1000 || saved_gid != 1000) {
            _exit(13);
        }
        if (getgroups(0, NULL) != 0) {
            _exit(14);
        }

        errno = 0;
        child_ret = syscall(SYS_unlinkat, dirfd, control_name, 0);
        child_errno = errno;
        if (child_ret != -1 || child_errno != EACCES) {
            _exit(15);
        }

        errno = 0;
        child_ret = syscall(SYS_unlinkat, dirfd, control_name, AT_REMOVEDIR);
        child_errno = errno;
        if (child_ret != -1 || child_errno != EACCES) {
            _exit(16);
        }
        _exit(0);
    }

    if (child > 0) {
        int status = 0;
        pid_t waited = waitpid(child, &status, 0);
        CHECK(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "uid/gid 1000 with no supplementary groups gets EACCES for both raw unlinkat flags");
    }

    errno = 0;
    ret = fstatat(dirfd, control_name, &st, 0);
    CHECK(ret == 0 && S_ISREG(st.st_mode),
          "rejected unlinkat calls preserve cgroup.procs regular file");
    errno = 0;
    ret = fstatat(AT_FDCWD, group_path, &st, 0);
    CHECK(ret == 0 && S_ISDIR(st.st_mode),
          "rejected unlinkat calls preserve isolated cgroup");

cleanup:
    if (dirfd >= 0) {
        close(dirfd);
    }
    if (group_created) {
        errno = 0;
        ret = chmod(group_path, 0755);
        CHECK(ret == 0, "restore isolated cgroup permissions for cleanup");
        if (ret == 0) {
            expect_rmdir_ok(group_path, "cleanup isolated unlink order cgroup");
        }
    }
}

static int cgroup_attr_matches(int dirfd, int controlfd, const char *group_path,
                               const char *alias_group_path,
                               dev_t dir_dev, dev_t alias_dir_dev, ino_t dir_ino,
                               dev_t control_dev, dev_t alias_control_dev,
                               ino_t control_ino,
                               uid_t dir_uid, gid_t dir_gid, mode_t dir_mode,
                               uid_t control_uid, gid_t control_gid,
                               mode_t control_mode, const char *stage)
{
    struct stat dir_fd_st, dir_lookup_st, dir_alias_st;
    struct stat control_fd_st, control_lookup_st, control_alias_st;
    int ok = 1;
    int ret;

#define VERIFY_ATTR(result, st, type, dev, ino, uid, gid, mode) do {       \
        int matches = (result) == 0 && S_IS##type((st).st_mode) &&        \
                      (st).st_dev == (dev) && (st).st_ino == (ino) &&     \
                      (st).st_uid == (uid) && (st).st_gid == (gid) &&     \
                      ((st).st_mode & 07777) == (mode);                   \
        CHECK(matches, stage);                                            \
        ok &= matches;                                                    \
    } while (0)

    ret = fstat(dirfd, &dir_fd_st);
    VERIFY_ATTR(ret, dir_fd_st, DIR, dir_dev, dir_ino,
                dir_uid, dir_gid, dir_mode);

    ret = stat(group_path, &dir_lookup_st);
    VERIFY_ATTR(ret, dir_lookup_st, DIR, dir_dev, dir_ino,
                dir_uid, dir_gid, dir_mode);

    ret = stat(alias_group_path, &dir_alias_st);
    VERIFY_ATTR(ret, dir_alias_st, DIR, alias_dir_dev, dir_ino,
                dir_uid, dir_gid, dir_mode);

    ret = fstat(controlfd, &control_fd_st);
    VERIFY_ATTR(ret, control_fd_st, REG, control_dev, control_ino,
                control_uid, control_gid, control_mode);

    ret = fstatat(dirfd, "cgroup.procs", &control_lookup_st, 0);
    VERIFY_ATTR(ret, control_lookup_st, REG, control_dev, control_ino,
                control_uid, control_gid, control_mode);

    char alias_control_path[sizeof("/tmp/cg-attrs-alias/attrs-owner/cgroup.procs")];
    snprintf(alias_control_path, sizeof(alias_control_path), "%s/cgroup.procs",
             alias_group_path);
    ret = stat(alias_control_path, &control_alias_st);
    VERIFY_ATTR(ret, control_alias_st, REG, alias_control_dev, control_ino,
                control_uid, control_gid, control_mode);
#undef VERIFY_ATTR
    return ok;
}

static void test_shared_cgroup_attributes(void)
{
    const char *group_path = CGROUP2_PATH "/attrs-owner";
    const char *alias_mount_path = "/tmp/cg-attrs-alias";
    const char *alias_group_path = "/tmp/cg-attrs-alias/attrs-owner";
    const char *control_path = CGROUP2_PATH "/attrs-owner/cgroup.procs";
    const char *alias_control_path = "/tmp/cg-attrs-alias/attrs-owner/cgroup.procs";
    struct stat dir_st, control_st;
    dev_t alias_dir_dev = 0, alias_control_dev = 0;
    mode_t initial_dir_mode, initial_control_mode;
    int group_created = 0, alias_dir_created = 0, alias_mounted = 0;
    int dirfd = -1, controlfd = -1;
    int ret;

    if (geteuid() != 0) {
        errno = 0;
        CHECK(0, "shared cgroup attributes test requires privileged root actor");
        return;
    }

    ret = mkdir(group_path, 0755);
    if (ret == 0) {
        group_created = 1;
    }
    CHECK(ret == 0, "create isolated cgroup for shared attribute test");
    if (ret != 0) {
        goto cleanup;
    }

    ret = stat(group_path, &dir_st);
    CHECK(ret == 0 && S_ISDIR(dir_st.st_mode),
          "isolated cgroup is a directory before attribute changes");
    if (ret != 0 || !S_ISDIR(dir_st.st_mode)) {
        goto cleanup;
    }
    ret = stat(control_path, &control_st);
    CHECK(ret == 0 && S_ISREG(control_st.st_mode),
          "cgroup.procs is a regular control file before attribute changes");
    if (ret != 0 || !S_ISREG(control_st.st_mode)) {
        goto cleanup;
    }

    ret = mkdir(alias_mount_path, 0755);
    if (ret == 0) {
        alias_dir_created = 1;
    }
    CHECK(ret == 0, "create independent cgroup2 alias mountpoint");
    if (ret != 0) {
        goto cleanup;
    }
    ret = mount("none", alias_mount_path, "cgroup2", 0, NULL);
    CHECK(ret == 0, "mount independent cgroup2 view for attribute checks");
    if (ret != 0) {
        goto cleanup;
    }
    alias_mounted = 1;

    ret = open(group_path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    dirfd = ret;
    CHECK(dirfd >= 0, "open isolated cgroup directory descriptor");
    if (dirfd < 0) {
        goto cleanup;
    }
    ret = open(control_path, O_RDONLY | O_CLOEXEC);
    controlfd = ret;
    CHECK(controlfd >= 0, "open cgroup.procs control file descriptor");
    if (controlfd < 0) {
        goto cleanup;
    }
    ret = fstat(dirfd, &control_st);
    CHECK(ret == 0 && S_ISDIR(control_st.st_mode),
          "read retained cgroup directory identity");
    if (ret != 0 || !S_ISDIR(control_st.st_mode)) {
        goto cleanup;
    }
    ret = stat(alias_group_path, &dir_st);
    CHECK(ret == 0 && S_ISDIR(dir_st.st_mode) &&
          dir_st.st_ino == control_st.st_ino,
          "alias mount resolves the same cgroup directory inode");
    if (ret != 0 || !S_ISDIR(dir_st.st_mode) ||
        dir_st.st_ino != control_st.st_ino) {
        goto cleanup;
    }
    alias_dir_dev = dir_st.st_dev;
    ret = fstat(controlfd, &control_st);
    CHECK(ret == 0 && S_ISREG(control_st.st_mode),
          "read retained cgroup.procs identity");
    if (ret != 0 || !S_ISREG(control_st.st_mode)) {
        goto cleanup;
    }
    ret = stat(alias_control_path, &dir_st);
    CHECK(ret == 0 && S_ISREG(dir_st.st_mode) &&
          dir_st.st_ino == control_st.st_ino,
          "alias mount resolves the same cgroup.procs inode");
    if (ret != 0 || !S_ISREG(dir_st.st_mode) ||
        dir_st.st_ino != control_st.st_ino) {
        goto cleanup;
    }
    alias_control_dev = dir_st.st_dev;
    ret = fstat(dirfd, &dir_st);
    if (ret != 0) {
        CHECK(0, "capture initial cgroup directory attributes");
        goto cleanup;
    }
    initial_dir_mode = dir_st.st_mode & 07777;
    ret = fstat(controlfd, &control_st);
    if (ret != 0) {
        CHECK(0, "capture initial cgroup.procs attributes");
        goto cleanup;
    }
    initial_control_mode = control_st.st_mode & 07777;

    ret = (int)syscall(SYS_fchown, dirfd, (uid_t)1000, (gid_t)1001);
    CHECK(ret == 0, "raw fchown sets cgroup directory owner to 1000:1001");
    if (ret != 0) {
        goto cleanup;
    }
    if (!cgroup_attr_matches(dirfd, controlfd, group_path, alias_group_path,
                             dir_st.st_dev, alias_dir_dev, dir_st.st_ino,
                             control_st.st_dev, alias_control_dev,
                             control_st.st_ino,
                             1000, 1001, initial_dir_mode,
                             control_st.st_uid, control_st.st_gid,
                             initial_control_mode,
                             "fchown directory visible through fd and both lookups")) {
        goto cleanup;
    }

    ret = (int)syscall(SYS_fchown, controlfd, (uid_t)1000, (gid_t)1001);
    CHECK(ret == 0, "raw fchown sets cgroup.procs owner to 1000:1001");
    if (ret != 0) {
        goto cleanup;
    }
    if (!cgroup_attr_matches(dirfd, controlfd, group_path, alias_group_path,
                             dir_st.st_dev, alias_dir_dev, dir_st.st_ino,
                             control_st.st_dev, alias_control_dev,
                             control_st.st_ino,
                             1000, 1001, initial_dir_mode, 1000, 1001,
                             initial_control_mode,
                             "fchown control owner visible through fd and both lookups")) {
        goto cleanup;
    }
    ret = (int)syscall(SYS_fchmod, dirfd, (mode_t)0751);
    CHECK(ret == 0, "raw fchmod sets cgroup directory mode to 0751");
    if (ret != 0) {
        goto cleanup;
    }
    if (!cgroup_attr_matches(dirfd, controlfd, group_path, alias_group_path,
                             dir_st.st_dev, alias_dir_dev, dir_st.st_ino,
                             control_st.st_dev, alias_control_dev,
                             control_st.st_ino,
                             1000, 1001, 0751, 1000, 1001,
                             initial_control_mode,
                             "fchmod directory visible through fd and both lookups")) {
        goto cleanup;
    }
    ret = (int)syscall(SYS_fchmod, controlfd, (mode_t)0640);
    CHECK(ret == 0, "raw fchmod sets cgroup.procs mode to 0640");
    if (ret != 0) {
        goto cleanup;
    }
    ret = fstat(dirfd, &dir_st);
    if (ret != 0) {
        CHECK(0, "read cgroup directory identity after initialization");
        goto cleanup;
    }
    ret = fstat(controlfd, &control_st);
    if (ret != 0) {
        CHECK(0, "read cgroup.procs identity after initialization");
        goto cleanup;
    }
    if (!cgroup_attr_matches(dirfd, controlfd, group_path, alias_group_path,
                             dir_st.st_dev, alias_dir_dev, dir_st.st_ino,
                             control_st.st_dev, alias_control_dev,
                             control_st.st_ino,
                             1000, 1001, 0751, 1000, 1001, 0640,
                             "initialized attributes visible through fd and both lookups")) {
        goto cleanup;
    }

    pid_t child = fork();
    CHECK(child >= 0, "fork ordinary cgroup attribute owner");
    if (child == 0) {
        uid_t real_uid, effective_uid, saved_uid;
        gid_t real_gid, effective_gid, saved_gid;
        if (setgroups(0, NULL) != 0 ||
            setresgid(1001, 1001, 1001) != 0 ||
            setresuid(1000, 1000, 1000) != 0 ||
            getresgid(&real_gid, &effective_gid, &saved_gid) != 0 ||
            getresuid(&real_uid, &effective_uid, &saved_uid) != 0 ||
            real_gid != 1001 || effective_gid != 1001 || saved_gid != 1001 ||
            real_uid != 1000 || effective_uid != 1000 || saved_uid != 1000 ||
            getgroups(0, NULL) != 0) {
            _exit(20);
        }
        if (syscall(SYS_fchmod, controlfd, (mode_t)0600) != 0) {
            _exit(21);
        }
        if (syscall(SYS_fchmod, dirfd, (mode_t)0750) != 0) {
            _exit(22);
        }
        errno = 0;
        long chown_ret = syscall(SYS_fchown, controlfd, (uid_t)0, (gid_t)-1);
        int chown_errno = errno;
        if (chown_ret != -1 || chown_errno != EPERM) {
            _exit(23);
        }
        if (fstat(dirfd, &dir_st) != 0 || !S_ISDIR(dir_st.st_mode) ||
            dir_st.st_uid != 1000 || dir_st.st_gid != 1001 ||
            (dir_st.st_mode & 07777) != 0750 ||
            fstat(controlfd, &control_st) != 0 || !S_ISREG(control_st.st_mode) ||
            control_st.st_uid != 1000 || control_st.st_gid != 1001 ||
            (control_st.st_mode & 07777) != 0600) {
            _exit(24);
        }
        _exit(0);
    }
    if (child < 0) {
        goto cleanup;
    }
    {
        int status = 0;
        pid_t waited = waitpid(child, &status, 0);
        CHECK(waited == child && WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "ordinary owner chmods both objects; chown is denied without side effects");
        if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            goto cleanup;
        }
    }
    if (!cgroup_attr_matches(dirfd, controlfd, group_path, alias_group_path,
                             dir_st.st_dev, alias_dir_dev, dir_st.st_ino,
                             control_st.st_dev, alias_control_dev,
                             control_st.st_ino,
                             1000, 1001, 0750, 1000, 1001, 0600,
                             "child changes and rejected chown persist through fd and both lookups")) {
        goto cleanup;
    }
    {
        struct stat primary_dir, alias_dir, primary_control, alias_control;
        int primary_dir_ret = fstat(dirfd, &primary_dir);
        int alias_dir_ret = stat(alias_group_path, &alias_dir);
        CHECK(primary_dir_ret == 0 && alias_dir_ret == 0 &&
              S_ISDIR(primary_dir.st_mode) && S_ISDIR(alias_dir.st_mode) &&
              primary_dir.st_dev == alias_dir.st_dev,
              "cgroup directory device number matches across cgroup2 mounts");
        int primary_control_ret = fstat(controlfd, &primary_control);
        int alias_control_ret = stat(alias_control_path, &alias_control);
        CHECK(primary_control_ret == 0 && alias_control_ret == 0 &&
              S_ISREG(primary_control.st_mode) && S_ISREG(alias_control.st_mode) &&
              primary_control.st_dev == alias_control.st_dev,
              "cgroup.procs device number matches across cgroup2 mounts");
    }

cleanup:
    if (controlfd >= 0) {
        ret = close(controlfd);
        CHECK(ret == 0, "close cgroup.procs attribute test descriptor");
    }
    if (dirfd >= 0) {
        ret = close(dirfd);
        CHECK(ret == 0, "close cgroup directory attribute test descriptor");
    }
    if (alias_mounted) {
        ret = umount(alias_mount_path);
        CHECK(ret == 0, "unmount independent cgroup2 alias view");
        if (ret == 0) {
            alias_mounted = 0;
        }
    }
    if (group_created) {
        ret = rmdir(group_path);
        CHECK(ret == 0, "remove isolated cgroup after shared attribute test");
    }
    if (alias_dir_created && !alias_mounted) {
        ret = rmdir(alias_mount_path);
        CHECK(ret == 0, "remove independent cgroup2 alias mountpoint");
    }
}

int main(void)
{
    TEST_START("cgroup-basic");

    expect_mkdir_errno("/sys", EEXIST,
                       "mkdir existing sysfs mount root fails with EEXIST");
    expect_mkdir_errno("/sys/fs", EEXIST,
                       "mkdir existing static sysfs directory fails with EEXIST");
    expect_mkdir_errno("/sys/fs/cgroup", EEXIST,
                       "mkdir existing sysfs cgroup mountpoint fails with EEXIST");

    check_mkdir(CGROUP2_PATH, "mkdir cgroup2 mountpoint");

    errno = 0;
    int ret = mount("none", CGROUP2_PATH, "cgroup2", 0, NULL);
    int mount_errno = errno;
    errno = mount_errno;
    CHECK(ret == 0, "mount cgroup2 succeeds");
    if (ret != 0) {
        printf("  OBSERVE | mount cgroup2 failed errno=%d (%s)\n",
               mount_errno, strerror(mount_errno));
        TEST_DONE();
    }

    expect_mkdir_errno(CGROUP2_PATH, EEXIST,
                       "mkdir cgroup2 mount root fails with EEXIST");

    test_unlink_control_file_order();
    test_shared_cgroup_attributes();

    char buf[4096];
    ssize_t nread = read_text_file(CGROUP2_PATH "/cgroup.procs", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.procs");
    if (nread >= 0) {
        CHECK(buffer_contains_pid(buf, getpid()),
              "root cgroup.procs contains current process");
    }

    nread = read_text_file(CGROUP2_PATH "/cgroup.controllers", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.controllers");
    if (nread >= 0) {
        CHECK(strstr(buf, "pids") != NULL,
              "root cgroup.controllers advertises the pids controller");
    }

    nread = read_text_file(CGROUP2_PATH "/cgroup.subtree_control", buf, sizeof(buf));
    CHECK(nread >= 0, "read root cgroup.subtree_control");
    if (nread >= 0) {
        CHECK(nread == 0, "root cgroup.subtree_control is initially empty");
    }

    expect_write_errno(CGROUP2_PATH "/cgroup.subtree_control", "+not-a-controller",
                       EINVAL, "unknown controller is rejected with EINVAL");

    expect_mkdir_ok(CGROUP2_PATH "/a", "mkdir child cgroup a succeeds");
    expect_path_exists(CGROUP2_PATH "/a", "child cgroup a exists");
    expect_empty_file(CGROUP2_PATH "/a/cgroup.procs",
                      "child cgroup.procs is empty before migration exists");
    expect_empty_file(CGROUP2_PATH "/a/cgroup.controllers",
                      "child cgroup.controllers is empty before pids is enabled");
    expect_empty_file(CGROUP2_PATH "/a/cgroup.subtree_control",
                      "child cgroup.subtree_control is initially empty");
    expect_path_missing(CGROUP2_PATH "/a/pids.max",
                        "child pids.max is hidden before parent enables pids");
    expect_path_missing(CGROUP2_PATH "/a/pids.current",
                        "child pids.current is hidden before parent enables pids");
    expect_path_missing(CGROUP2_PATH "/a/pids.peak",
                        "child pids.peak is hidden before parent enables pids");
    expect_path_missing(CGROUP2_PATH "/a/pids.events",
                        "child pids.events is hidden before parent enables pids");
    expect_mkdir_errno(CGROUP2_PATH "/a", EEXIST,
                       "duplicate mkdir child cgroup fails with EEXIST");

    expect_mkdir_ok(CGROUP2_PATH "/a/b", "mkdir nested child cgroup succeeds");
    expect_rmdir_errno(CGROUP2_PATH "/a", ENOTEMPTY,
                       "rmdir cgroup with child fails with ENOTEMPTY");
    expect_path_exists(CGROUP2_PATH "/a", "parent cgroup remains after failed rmdir");
    expect_path_exists(CGROUP2_PATH "/a/b", "child cgroup remains after failed parent rmdir");
    expect_rmdir_ok(CGROUP2_PATH "/a/b", "rmdir empty nested cgroup succeeds");
    expect_path_missing(CGROUP2_PATH "/a/b", "removed nested cgroup is missing");
    expect_rmdir_ok(CGROUP2_PATH "/a", "rmdir empty child cgroup succeeds");
    expect_path_missing(CGROUP2_PATH "/a", "removed child cgroup is missing");

    expect_mkdir_ok(CGROUP2_PATH "/into",
                    "mkdir clone3 target cgroup succeeds");
    expect_clone_into_cgroup(CGROUP2_PATH "/into",
                             "clone3 CLONE_INTO_CGROUP starts child in target cgroup");
    expect_rmdir_ok(CGROUP2_PATH "/into",
                    "cleanup clone3 target cgroup succeeds after child exit");

    expect_mkdir_ok(CGROUP2_PATH "/cwd-cache", "mkdir cgroup for cwd cache regression");
    expect_chdir_ok(CGROUP2_PATH "/cwd-cache", "chdir into cgroup for cwd cache regression");
    expect_mkdir_ok("b", "relative mkdir child cgroup from cwd succeeds");
    expect_rmdir_ok(CGROUP2_PATH "/cwd-cache/b",
                    "absolute rmdir removes child created from cwd");
    expect_open_dir_errno("b", ENOENT,
                          "relative open after absolute rmdir does not return stale cgroup");
    expect_chdir_ok("/", "restore cwd after cwd cache regression");
    expect_rmdir_ok(CGROUP2_PATH "/cwd-cache", "cleanup cwd cache regression cgroup");

    expect_open_create_errno(CGROUP2_PATH "/user.file", EPERM,
                             "open O_CREAT regular file in cgroupfs fails with EPERM");
    expect_path_missing(CGROUP2_PATH "/user.file",
                        "failed open O_CREAT leaves no regular file");
    expect_link_errno(CGROUP2_PATH "/cgroup.procs", CGROUP2_PATH "/procs.link",
                      EPERM, "hard link in cgroupfs fails with EPERM");
    expect_path_missing(CGROUP2_PATH "/procs.link",
                        "failed hard link leaves no new path");
    expect_symlink_errno("x", CGROUP2_PATH "/sym", EPERM,
                         "symlink in cgroupfs fails with EPERM");
    expect_path_missing(CGROUP2_PATH "/sym", "failed symlink leaves no new path");

    expect_mkdir_ok(CGROUP2_PATH "/ren", "mkdir cgroup for rename negative test succeeds");
    expect_rename_errno(CGROUP2_PATH "/ren", CGROUP2_PATH "/renamed", EPERM,
                        "rename cgroup directory fails with EPERM");
    expect_path_exists(CGROUP2_PATH "/ren", "rename failure keeps original cgroup");
    expect_path_missing(CGROUP2_PATH "/renamed", "rename failure leaves destination missing");
    expect_rmdir_ok(CGROUP2_PATH "/ren", "cleanup rename negative test cgroup");

    check_mkdir(CGROUP_V1_PATH, "mkdir cgroup v1 mountpoint");

    errno = 0;
    ret = mount("none", CGROUP_V1_PATH, "cgroup", 0, NULL);
    int v1_errno = errno;
    errno = v1_errno;
    CHECK(ret == -1 && v1_errno == ENODEV, "mount cgroup v1 fails with ENODEV");

    TEST_DONE();
}
