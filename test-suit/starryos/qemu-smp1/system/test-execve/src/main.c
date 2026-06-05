/*
 * Focused StarryOS conformance test for the execve(2) family — execve() and
 * execveat(). fork() and wait4() appear only as scaffolding: each exec runs
 * in a forked child so the test harness survives, and wait4() collects the
 * child's exit status to confirm the image was actually replaced.
 *
 * execve() cases: replace a child with a shell or report ENOENT, observed via
 * the child's exit status.
 *
 * execveat() cases (Linux funnels both through do_execveat_common) exercise
 * the resolution modes it adds on top of execve — a relative path against a
 * directory fd or AT_FDCWD, an absolute path, AT_EMPTY_PATH — plus the
 * EINVAL/EBADF/ENOTDIR/ENOENT error returns.
 *
 * This is intentionally narrower than linux-compatible-testsuit's
 * test_fork_v2.c, which also covers clone/clone3, fd inheritance,
 * copy-on-write, session/process-group behavior, and more wait4 modes.
 */
#include "test_framework.h"

#include <fcntl.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#ifndef SYS_execveat
#if defined(__x86_64__)
#define SYS_execveat 358
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_execveat 281
#else
#error "SYS_execveat is unknown for this architecture"
#endif
#endif

#ifndef AT_EMPTY_PATH
#define AT_EMPTY_PATH 0x1000
#endif

static void check_exited_with(int status, int expected_code, const char *msg)
{
    CHECK(WIFEXITED(status), msg);
    if (WIFEXITED(status)) {
        CHECK(WEXITSTATUS(status) == expected_code, "child exit status matches");
    }
}

static void test_fork_child_exit_wait4(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork creates a child process");
    if (pid < 0) {
        return;
    }

    if (pid == 0) {
        _exit(42);
    }

    int status = 0;
    // resource of sub process
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 returns the forked child pid");
    if (waited == pid) {
        check_exited_with(status, 42, "forked child exits normally");
    }
}

static void test_fork_execve_shell_exit_wait4(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork before execve succeeds");
    if (pid < 0) {
        return;
    }

    if (pid == 0) {
        char *const argv[] = { "/bin/sh", "-c", "exit 7", NULL };
        execve("/bin/sh", argv, environ);
        _exit(126);
    }

    int status = 0;
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 observes execve child");
    if (waited == pid) {
        check_exited_with(status, 7, "execve child exits with shell status");
    }
}

static void test_execve_missing_path_reports_enoent(void)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork for failing execve succeeds");
    if (pid < 0) {
        return;
    }

    if (pid == 0) {
        char *const argv[] = { "/no-such-starry-test-binary", NULL };
        char *const envp[] = { NULL };
        execve(argv[0], argv, envp);
        _exit(errno == ENOENT ? 127 : 126);
    }

    int status = 0;
    struct rusage usage;
    errno = 0;
    pid_t waited = wait4(pid, &status, 0, &usage);
    CHECK(waited == pid, "wait4 observes child after failed execve");
    if (waited == pid) {
        check_exited_with(status, 127, "execve missing path reports ENOENT in child");
    }
}

/* The shell command whose exit status proves the new image really ran. */
#define EXECVEAT_OK_STATUS 42
static char *const SH_ARGV[] = {"sh", "-c", "exit 42", NULL};

static long do_execveat(int dirfd, const char *path, int flags)
{
    return syscall(SYS_execveat, dirfd, path, SH_ARGV, environ, flags);
}

/*
 * Run an execveat() expected to succeed, in a child, and return its wait
 * status. The child only reaches _exit(126) if exec returned (i.e. failed).
 */
static int execveat_success_status(int dirfd, const char *path, int flags)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork before execveat succeeds");
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        do_execveat(dirfd, path, flags);
        _exit(126);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    CHECK(waited == pid, "waitpid collects the execveat child");
    return (waited == pid) ? status : -1;
}

/*
 * Run an execveat() expected to fail, in a child, and return the errno it
 * reported (as the child's exit code). Running in a child keeps a wrongly
 * succeeding exec from replacing this test harness.
 */
static int execveat_failure_errno(int dirfd, const char *path, int flags)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork before failing execveat succeeds");
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        do_execveat(dirfd, path, flags);
        _exit(errno);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    if (waited != pid || !WIFEXITED(status)) {
        return -1;
    }
    return WEXITSTATUS(status);
}

static void check_execveat_ran(int status, const char *msg)
{
    CHECK(WIFEXITED(status), msg);
    if (WIFEXITED(status)) {
        CHECK(WEXITSTATUS(status) == EXECVEAT_OK_STATUS,
              "execveat child exits with the shell's status");
    }
}

static void test_execveat_relative_path_via_dirfd(void)
{
    int bin = open("/bin", O_RDONLY | O_DIRECTORY);
    CHECK(bin >= 0, "open /bin as a directory fd");
    if (bin < 0) {
        return;
    }
    int status = execveat_success_status(bin, "sh", 0);
    check_execveat_ran(status, "execveat resolves a relative path against dirfd");
    close(bin);
}

static void test_execveat_absolute_path_ignores_dirfd(void)
{
    /* 999 is not an open fd: an absolute pathname must ignore dirfd entirely. */
    int status = execveat_success_status(999, "/bin/sh", 0);
    check_execveat_ran(status, "execveat absolute path ignores dirfd");
}

static void test_execveat_relative_path_via_fdcwd(void)
{
    char cwd[256];
    CHECK(getcwd(cwd, sizeof(cwd)) != NULL, "snapshot cwd");
    CHECK(chdir("/bin") == 0, "chdir into /bin");

    int status = execveat_success_status(AT_FDCWD, "sh", 0);
    check_execveat_ran(status,
                       "execveat resolves a relative path against AT_FDCWD");

    CHECK(chdir(cwd) == 0, "restore cwd");
}

static void test_execveat_at_empty_path_executes_fd(void)
{
    int fd = open("/bin/sh", O_RDONLY);
    CHECK(fd >= 0, "open /bin/sh for AT_EMPTY_PATH exec");
    if (fd < 0) {
        return;
    }
    int status = execveat_success_status(fd, "", AT_EMPTY_PATH);
    check_execveat_ran(status, "execveat AT_EMPTY_PATH executes the open fd");
    close(fd);
}

static void test_execveat_error_returns(void)
{
    /* 0x4 is outside the accepted AT_EMPTY_PATH|AT_SYMLINK_NOFOLLOW set. */
    CHECK(execveat_failure_errno(AT_FDCWD, "/bin/sh", 0x4) == EINVAL,
          "execveat rejects unknown flag bits with EINVAL");
    CHECK(execveat_failure_errno(999, "sh", 0) == EBADF,
          "execveat relative path against a closed dirfd returns EBADF");
    CHECK(execveat_failure_errno(AT_FDCWD, "/no-such-starry-execveat", 0)
              == ENOENT,
          "execveat missing program returns ENOENT");
}

static void test_execveat_non_directory_dirfd_enotdir(void)
{
    int fd = open("/bin/sh", O_RDONLY);
    CHECK(fd >= 0, "open /bin/sh as a non-directory fd");
    if (fd < 0) {
        return;
    }
    CHECK(execveat_failure_errno(fd, "sh", 0) == ENOTDIR,
          "execveat relative path against a non-directory fd returns ENOTDIR");
    close(fd);
}

int main(void)
{
    TEST_START("execve/execveat family semantics");

    test_fork_child_exit_wait4();
    test_fork_execve_shell_exit_wait4();
    test_execve_missing_path_reports_enoent();

    test_execveat_relative_path_via_dirfd();
    test_execveat_absolute_path_ignores_dirfd();
    test_execveat_relative_path_via_fdcwd();
    test_execveat_at_empty_path_executes_fd();
    test_execveat_error_returns();
    test_execveat_non_directory_dirfd_enotdir();

    TEST_DONE();
}
