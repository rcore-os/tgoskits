#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <stdint.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_openat2
#define SYS_openat2 437
#endif

#define DIR_PATH "/tmp/dac-open"
#define SECRET DIR_PATH "/secret"
#define LOCKED_DIR DIR_PATH "/locked"
#define CREATED DIR_PATH "/created"
#define CREATED_NO_EXCL DIR_PATH "/created-no-excl"
#define CONTENT "hello-dac"
#define CONTENT_LEN 9

#define USER_ID 1000
#define CAP_VERSION_3 0x20080522
#define CAP_DAC_OVERRIDE_BIT 1
#define RESOLVE_NO_SYMLINKS_FLAG 0x04
#define RESOLVE_BENEATH_FLAG 0x08

struct cap_header {
    uint32_t version;
    int pid;
};

struct cap_data {
    uint32_t effective;
    uint32_t permitted;
    uint32_t inheritable;
};

struct open_how_args {
    uint64_t flags;
    uint64_t mode;
    uint64_t resolve;
};

/* Opens and closes at once; the result is the open(2) return value with errno preserved. */
static int try_open(const char *path, int flags)
{
    int fd = open(path, flags, 0644);
    if (fd >= 0)
        close(fd);
    return fd < 0 ? -1 : 0;
}

static int try_openat2(int dirfd, const char *name, int flags)
{
    struct open_how_args how = {
        .flags = (uint64_t)flags,
        .resolve = RESOLVE_BENEATH_FLAG | RESOLVE_NO_SYMLINKS_FLAG,
    };
    int fd = (int)syscall(SYS_openat2, dirfd, name, &how, sizeof(how));
    if (fd >= 0)
        close(fd);
    return fd < 0 ? -1 : 0;
}

static long secret_size(void)
{
    struct stat st;
    return stat(SECRET, &st) == 0 ? (long)st.st_size : -1;
}

static void become_user(void)
{
    CHECK(setgid(USER_ID) == 0 && setuid(USER_ID) == 0, "drop to uid/gid 1000");
    CHECK_RET(prctl(PR_SET_DUMPABLE, 1), 0, "keep /proc/self owned by the task");
}

static void in_child(const char *name, void (*body)(void))
{
    fflush(stdout);
    int failed_before = __fail;
    pid_t child = fork();
    if (child == 0) {
        body();
        fflush(stdout);
        _exit(__fail > failed_before);
    }
    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child, name);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, name);
}

/* Other has read but not write on the root-owned 0644 file. */
static void plain_open_checks_mode(void)
{
    become_user();
    CHECK_RET(try_open(SECRET, O_RDONLY), 0, "O_RDONLY allowed by the other read bit");
    CHECK_RET(try_open(SECRET, O_RDONLY | O_APPEND), 0, "O_RDONLY|O_APPEND only needs read");
    CHECK_ERR(try_open(SECRET, O_WRONLY), EACCES, "O_WRONLY refused");
    CHECK_ERR(try_open(SECRET, O_RDWR), EACCES, "O_RDWR refused");
    CHECK_ERR(try_open(SECRET, O_WRONLY | O_TRUNC), EACCES, "O_WRONLY|O_TRUNC refused");
    CHECK_ERR(try_open(SECRET, O_RDONLY | O_TRUNC), EACCES, "O_RDONLY|O_TRUNC needs write");
    CHECK_ERR(try_open(SECRET, O_CREAT | O_WRONLY), EACCES, "O_CREAT on an existing file still checks the mode");
    CHECK_RET(secret_size(), CONTENT_LEN, "refused truncations left the file intact");

    int fd = open(CREATED, O_CREAT | O_EXCL | O_WRONLY, 0444);
    CHECK(fd >= 0, "the creator may write a file it creates with mode 0444");
    if (fd >= 0) {
        CHECK_RET(write(fd, "ok", 2), 2, "write through the creating descriptor");
        close(fd);
    }
    fd = open(CREATED_NO_EXCL, O_CREAT | O_WRONLY, 0444);
    CHECK(fd >= 0, "O_CREAT without O_EXCL also skips the check for a new file");
    if (fd >= 0)
        close(fd);
    CHECK_ERR(try_open(CREATED, O_WRONLY), EACCES, "reopening the 0444 file for write is refused");
}

/* Linux reports the node type before evaluating permission bits. */
static void type_errors_come_first(void)
{
    become_user();
    CHECK_ERR(try_open(LOCKED_DIR, O_WRONLY), EISDIR, "O_WRONLY on a 0555 directory reports EISDIR");
    CHECK_ERR(try_open(LOCKED_DIR, O_RDONLY | O_TRUNC), EISDIR, "O_TRUNC on a directory reports EISDIR");
    CHECK_ERR(try_open(SECRET "/", O_WRONLY), ENOTDIR, "a trailing slash on a file reports ENOTDIR");
}

/* A descriptor opened before dropping privileges must not reopen with more access. */
static void procfd_reopen_checks_mode(void)
{
    int fd = open(SECRET, O_RDONLY);
    CHECK(fd >= 0, "root opens the file read-only");
    become_user();
    char proc_path[64];
    char dev_path[64];
    snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
    snprintf(dev_path, sizeof(dev_path), "/dev/fd/%d", fd);
    CHECK_RET(try_open(proc_path, O_RDONLY), 0, "reopen read-only through /proc/self/fd");
    CHECK_ERR(try_open(proc_path, O_WRONLY), EACCES, "reopen for write through /proc/self/fd refused");
    CHECK_ERR(try_open(proc_path, O_RDWR | O_TRUNC), EACCES, "reopen with O_TRUNC through /proc/self/fd refused");
    CHECK_ERR(try_open(dev_path, O_WRONLY), EACCES, "reopen for write through /dev/fd refused");
    CHECK_RET(secret_size(), CONTENT_LEN, "refused reopen left the file intact");
}

static void openat2_resolve_checks_mode(void)
{
    become_user();
    int dirfd = open(DIR_PATH, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "open the test directory");
    CHECK_RET(try_openat2(dirfd, "secret", O_RDONLY), 0, "openat2 with resolve flags allows read");
    CHECK_ERR(try_openat2(dirfd, "secret", O_WRONLY), EACCES, "openat2 with resolve flags refuses write");
    CHECK_ERR(try_openat2(dirfd, "secret", O_WRONLY | O_TRUNC), EACCES, "openat2 with resolve flags refuses O_TRUNC");
    CHECK_RET(secret_size(), CONTENT_LEN, "refused openat2 left the file intact");
}

/* A non-root task that keeps CAP_DAC_OVERRIDE bypasses the mode bits. */
static void kept_dac_override_allows_write(void)
{
    CHECK_RET(prctl(PR_SET_KEEPCAPS, 1), 0, "keep capabilities across setuid");
    become_user();
    struct cap_header header = {.version = CAP_VERSION_3, .pid = 0};
    struct cap_data data[2] = {{0}};
    data[0].effective = 1u << CAP_DAC_OVERRIDE_BIT;
    data[0].permitted = 1u << CAP_DAC_OVERRIDE_BIT;
    CHECK_RET(syscall(SYS_capset, &header, data), 0, "raise CAP_DAC_OVERRIDE as uid 1000");
    CHECK_RET(try_open(SECRET, O_WRONLY), 0, "CAP_DAC_OVERRIDE allows O_WRONLY without the write bit");
}

int main(void)
{
    TEST_START("open enforces discretionary access on existing files");
    CHECK(getuid() == 0, "runs as root so it can drop privileges");

    mkdir(DIR_PATH, 0777);
    CHECK_RET(chmod(DIR_PATH, 0777), 0, "test directory is writable by everyone");
    unlink(CREATED);
    unlink(CREATED_NO_EXCL);
    mkdir(LOCKED_DIR, 0555);
    CHECK_RET(chmod(LOCKED_DIR, 0555), 0, "locked directory is mode 0555");
    int fd = open(SECRET, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "create the root-owned file");
    if (fd < 0) {
        TEST_DONE();
    }
    CHECK_RET(write(fd, CONTENT, CONTENT_LEN), CONTENT_LEN, "write the initial content");
    close(fd);
    CHECK_RET(chmod(SECRET, 0644), 0, "file is mode 0644");
    CHECK_RET(try_open(SECRET, O_RDWR), 0, "root opens the file read-write");

    in_child("plain open checks the mode", plain_open_checks_mode);
    in_child("type errors come before permission errors", type_errors_come_first);
    in_child("procfd reopen checks the mode", procfd_reopen_checks_mode);
    in_child("openat2 with resolve flags checks the mode", openat2_resolve_checks_mode);
    in_child("kept CAP_DAC_OVERRIDE allows write", kept_dac_override_allows_write);
    TEST_DONE();
}
