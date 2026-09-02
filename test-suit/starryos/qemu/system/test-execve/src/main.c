#define _GNU_SOURCE

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
#include <pthread.h>
#include <sched.h>
#include <stdbool.h>
#include <stdatomic.h>
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

#ifndef SYS_memfd_create
#if defined(__x86_64__)
#define SYS_memfd_create 319
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__)
#define SYS_memfd_create 279
#else
#error "SYS_memfd_create is unknown for this architecture"
#endif
#endif

#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002U
#endif
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
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

/* Copy every byte of `src_path` into the already-open `dst_fd`. */
static int copy_file_into(int dst_fd, const char *src_path)
{
    int src = open(src_path, O_RDONLY);
    if (src < 0) {
        return -1;
    }
    char buf[4096];
    ssize_t n;
    while ((n = read(src, buf, sizeof(buf))) > 0) {
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(dst_fd, buf + off, (size_t)(n - off));
            if (w <= 0) {
                close(src);
                return -1;
            }
            off += w;
        }
    }
    close(src);
    return n < 0 ? -1 : 0;
}

/*
 * systemd's pattern: stage an executable into an anonymous memfd, seal it
 * write-only-once, then execveat(fd, "", AT_EMPTY_PATH). Sealing with
 * F_SEAL_WRITE before exec mirrors systemd and is what Linux's
 * deny_write_access requires (an unsealed, still-writable memfd would
 * otherwise exec with ETXTBSY), so this case passes identically on real Linux.
 */
static void test_execveat_memfd_sealed_exec(void)
{
    int mfd = (int)syscall(SYS_memfd_create, "execve-test", MFD_ALLOW_SEALING);
    CHECK(mfd >= 0, "memfd_create for execveat");
    if (mfd < 0) {
        return;
    }

    CHECK(copy_file_into(mfd, "/bin/sh") == 0, "stage /bin/sh into the memfd");
    CHECK(fcntl(mfd, F_ADD_SEALS, F_SEAL_WRITE) == 0,
          "seal the memfd write access before exec");

    int status = execveat_success_status(mfd, "", AT_EMPTY_PATH);
    check_execveat_ran(status,
                       "execveat AT_EMPTY_PATH executes a sealed memfd image");
    close(mfd);
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

/*
 * Each payload string and the pointer vectors below are individually valid.
 * Their aggregate is deliberately just over StarryOS's 2 MiB execve budget.
 * The child uses a shell exit status that differs from the E2BIG sentinel, so
 * a missing limit cannot accidentally look like a passing test.
 */
#define EXECVE_PAYLOAD_BYTES 8192
#define EXECVE_LIMIT_PAYLOAD_COUNT 257

static int execve_payload_status(size_t argv_payload_count, size_t envp_payload_count)
{
    pid_t pid = fork();
    if (pid < 0) {
        return -1;
    }
    if (pid == 0) {
        char *payload = malloc(EXECVE_PAYLOAD_BYTES);
        char **argv = calloc(argv_payload_count + 4, sizeof(*argv));
        char **envp = calloc(envp_payload_count + 1, sizeof(*envp));
        if (payload == NULL || argv == NULL || envp == NULL) {
            _exit(125);
        }

        memset(payload, 'x', EXECVE_PAYLOAD_BYTES - 1);
        payload[0] = 'X';
        payload[1] = '=';
        payload[EXECVE_PAYLOAD_BYTES - 1] = '\0';

        argv[0] = "/bin/sh";
        argv[1] = "-c";
        argv[2] = "exit 42";
        for (size_t i = 0; i < argv_payload_count; ++i) {
            argv[i + 3] = payload;
        }
        for (size_t i = 0; i < envp_payload_count; ++i) {
            envp[i] = payload;
        }

        execve("/bin/sh", argv, envp);
        _exit(errno == E2BIG ? 0 : 1);
    }

    int status = 0;
    return waitpid(pid, &status, 0) == pid && WIFEXITED(status)
               ? WEXITSTATUS(status)
               : -1;
}

static void test_execve_aggregate_argument_budget(void)
{
    CHECK(execve_payload_status(EXECVE_LIMIT_PAYLOAD_COUNT, 0) == 0,
          "execve rejects aggregate argv bytes with E2BIG");
    CHECK(execve_payload_status(0, EXECVE_LIMIT_PAYLOAD_COUNT) == 0,
          "execve rejects aggregate envp bytes with E2BIG");
    CHECK(execve_payload_status(EXECVE_LIMIT_PAYLOAD_COUNT / 2,
                               EXECVE_LIMIT_PAYLOAD_COUNT / 2) == 0,
          "execve applies one shared argv/envp byte budget");
}

#define EXEC_ASPACE_RACE_ROUNDS 32
#define EXEC_ASPACE_RACE_THREADS 8

struct exec_aspace_race {
    atomic_uint ready_threads;
    atomic_bool keep_running;
    long online_cpus;
};

static void *exec_aspace_sibling(void *arg)
{
    struct exec_aspace_race *race = arg;
    unsigned int sibling = atomic_fetch_add_explicit(
        &race->ready_threads, 1, memory_order_release);

    if (race->online_cpus > 1) {
        cpu_set_t cpus;
        CPU_ZERO(&cpus);
        CPU_SET((int)(sibling % (unsigned int)race->online_cpus), &cpus);
        (void)pthread_setaffinity_np(pthread_self(), sizeof(cpus), &cpus);
    }

    while (atomic_load_explicit(&race->keep_running, memory_order_acquire)) {
        (void)syscall(SYS_gettid);
        sched_yield();
    }
    return NULL;
}

/*
 * A successful exec destroys every sibling thread. The retiring siblings may
 * already be absent from the thread-group registry while still finishing
 * their final kernel return on another CPU. Repeating this interleaving makes
 * the page-table ownership boundary observable: the new image must run and
 * exit cleanly every time, without an instruction-page-fault loop or hang.
 */
static void test_multithread_exec_keeps_retiring_page_table_roots_alive(void)
{
    int failed_round = -1;
    int failed_status = 0;

    for (int round = 0; round < EXEC_ASPACE_RACE_ROUNDS; ++round) {
        pid_t pid = fork();
        if (pid < 0) {
            failed_round = round;
            failed_status = errno;
            break;
        }
        if (pid == 0) {
            struct exec_aspace_race race;
            pthread_t siblings[EXEC_ASPACE_RACE_THREADS];
            atomic_init(&race.ready_threads, 0);
            atomic_init(&race.keep_running, true);
            race.online_cpus = sysconf(_SC_NPROCESSORS_ONLN);

            int created = 0;
            for (; created < EXEC_ASPACE_RACE_THREADS; ++created) {
                int create_error = pthread_create(&siblings[created], NULL,
                                                  exec_aspace_sibling, &race);
                if (create_error != 0) {
                    fprintf(stderr,
                            "multithread exec pthread_create failed: "
                            "round=%d created=%d error=%d\n",
                            round, created, create_error);
                    atomic_store_explicit(&race.keep_running, false,
                                          memory_order_release);
                    for (int i = 0; i < created; ++i) {
                        pthread_join(siblings[i], NULL);
                    }
                    _exit(125);
                }
            }
            while (atomic_load_explicit(&race.ready_threads,
                                        memory_order_acquire)
                   != EXEC_ASPACE_RACE_THREADS) {
                sched_yield();
            }

            char *const argv[] = { "/bin/true", NULL };
            syscall(SYS_execve, argv[0], argv, environ);

            atomic_store_explicit(&race.keep_running, false,
                                  memory_order_release);
            for (int i = 0; i < created; ++i) {
                pthread_join(siblings[i], NULL);
            }
            _exit(126);
        }

        int status = 0;
        if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status)
            || WEXITSTATUS(status) != 0) {
            failed_round = round;
            failed_status = status;
            break;
        }
    }

    if (failed_round >= 0) {
        fprintf(stderr, "multithread exec failed at round %d status=%d\n",
                failed_round, failed_status);
    }
    CHECK(failed_round < 0,
          "multithread exec keeps retiring siblings' page-table roots alive");
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
    test_execveat_memfd_sealed_exec();
    test_execveat_error_returns();
    test_execveat_non_directory_dirfd_enotdir();
    test_execve_aggregate_argument_budget();
    test_multithread_exec_keeps_retiring_page_table_roots_alive();

    TEST_DONE();
}
