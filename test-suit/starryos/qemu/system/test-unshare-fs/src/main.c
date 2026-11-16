/*
 * test-unshare-fs — verify unshare(CLONE_FS).
 *
 * nixpkgs 测试可能用到 — unshare(CLONE_FS) 是 Nix fetchTarball
 * 下载线程的前置依赖。
 *
 * Scenarios:
 *   1. unshare(CLONE_FS) on independent task → returns 0.
 *   2. unshare(CLONE_FILES) in one thread leaves sibling fd tables unchanged.
 *   3. missing namespace authority leaves FD, FS, and namespace state unchanged.
 *   4. combined unshare preserves deleted cwd and isolates sibling state.
 *   5. a pending CLONE_NEWPID survives a later namespace unshare.
 *   6. clone(CLONE_FS) → share cwd → child unshare(CLONE_FS) → cwd
 *      isolation: child chdir must not affect parent cwd.
 *   7. Low- and high-word unknown unshare flags → EINVAL.
 *
 * Note: uses clone(CLONE_FS | SIGCHLD), NOT fork().  In this kernel
 * fork() does NOT share FS_CONTEXT, so a fork-based test would pass
 * even when CLONE_FS sharing + unshare isolation is broken.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define fail(fmt, ...) do { \
    fprintf(stderr, "FAIL | %s:%d | " fmt "\n", __FILE__, __LINE__, ##__VA_ARGS__); \
    exit(1); \
} while(0)

#define pass(fmt, ...) \
    printf("  PASS | %s:%d | " fmt "\n", __FILE__, __LINE__, ##__VA_ARGS__)

#define check(cond, fmt, ...) do { \
    if (cond) pass(fmt, ##__VA_ARGS__); else fail(fmt, ##__VA_ARGS__); \
} while(0)

/* Minimal stack for clone(CLONE_FS) child — 64KiB should be enough for
   chdir / getcwd / _exit; SIGCHLD so waitpid works. */
#define STACK_SIZE (64 * 1024)
#define UNSHARE_SYNC_TIMEOUT_SECONDS 5

struct clone_arg {
    int *shared;
    int barrier;
};

struct files_unshare_arg {
    int fd;
    int unshare_rc;
    int unshare_errno;
    int close_rc;
};

struct unshare_observer {
    pthread_mutex_t mutex;
    pthread_cond_t condition;
    int fd;
    int ready;
    int check_state;
    int fd_state;
    int close_rc;
    char cwd[256];
    int cwd_ok;
    int cwd_errno;
    int observer_wait_error;
};

/* Linux capability ABI v3; minimal guest sysroots omit Linux UAPI headers. */
struct cap_header { uint32_t version; int32_t pid; };
struct cap_data { uint32_t effective, permitted, inheritable; };

static int wait_for_observer_flag(pthread_cond_t *condition,
                                  pthread_mutex_t *mutex,
                                  const int *flag)
{
    struct timespec deadline;
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0)
        return errno != 0 ? errno : EIO;
    deadline.tv_sec += UNSHARE_SYNC_TIMEOUT_SECONDS;

    while (*flag == 0) {
        int error = pthread_cond_timedwait(condition, mutex, &deadline);
        if (error != 0)
            return error;
    }
    return 0;
}

static void *unshare_files_thread(void *opaque) {
    struct files_unshare_arg *arg = opaque;

    errno = 0;
    arg->unshare_rc = unshare(CLONE_FILES);
    arg->unshare_errno = errno;
    arg->close_rc = arg->unshare_rc == 0 ? close(arg->fd) : -1;
    return NULL;
}

static void test_thread_unshare_files_isolation(void) {
    int fd = open("/dev/null", O_RDONLY);
    check(fd >= 0, "open fd shared by pthreads");

    struct files_unshare_arg arg = {
        .fd = fd,
        .unshare_rc = -1,
        .unshare_errno = 0,
        .close_rc = -1,
    };
    pthread_t thread;
    int rc = pthread_create(&thread, NULL, unshare_files_thread, &arg);
    check(rc == 0, "create CLONE_FILES-sharing pthread (rc=%d)", rc);
    if (rc == 0)
        check(pthread_join(thread, NULL) == 0, "join CLONE_FILES pthread");

    check(arg.unshare_rc == 0,
          "pthread unshare(CLONE_FILES) succeeds (rc=%d, errno=%d)",
          arg.unshare_rc, arg.unshare_errno);
    check(arg.close_rc == 0, "pthread closes fd in its private table");

    errno = 0;
    check(fcntl(fd, F_GETFD) >= 0,
          "calling thread retains fd after sibling unshare+close (errno=%d)", errno);
    close(fd);

    printf("UNSHARE_FILES_THREAD_ISOLATION_PASSED\n");
}

static void *observe_shared_state_after_unshare(void *opaque) {
    struct unshare_observer *arg = opaque;

    pthread_mutex_lock(&arg->mutex);
    arg->ready = 1;
    pthread_cond_signal(&arg->condition);
    arg->observer_wait_error = wait_for_observer_flag(
        &arg->condition, &arg->mutex, &arg->check_state);
    pthread_mutex_unlock(&arg->mutex);

    if (arg->observer_wait_error != 0)
        return NULL;

    errno = 0;
    int flags = fcntl(arg->fd, F_GETFD);
    arg->fd_state = flags >= 0 ? 1 : errno == EBADF ? 0 : -1;
    arg->close_rc = flags >= 0 ? close(arg->fd) : 0;
    errno = 0;
    arg->cwd_ok = getcwd(arg->cwd, sizeof(arg->cwd)) != NULL;
    arg->cwd_errno = errno;
    return NULL;
}

static ino_t current_namespace_inode(const char *name) {
    char path[64];
    int length = snprintf(path, sizeof(path), "/proc/self/ns/%s", name);
    check(length > 0 && (size_t)length < sizeof(path), "build namespace path");
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    check(fd >= 0, "open current %s namespace", name);
    struct stat st;
    check(fstat(fd, &st) == 0, "stat current %s namespace", name);
    close(fd);
    return st.st_ino;
}

static void test_combined_unshare_state(void) {
    for (int expect_success = 0; expect_success <= 1; ++expect_success) {
        ino_t original_uts_inode = current_namespace_inode("uts");
        ino_t original_mnt_inode = current_namespace_inode("mnt");
        int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
        check(fd >= 0, "open fd shared across combined unshare");

        struct unshare_observer arg = {
            .mutex = PTHREAD_MUTEX_INITIALIZER,
            .fd = fd,
        };
        pthread_condattr_t condition_attr;
        check(pthread_condattr_init(&condition_attr) == 0,
              "initialize unshare observer condition attributes");
        check(pthread_condattr_setclock(&condition_attr, CLOCK_MONOTONIC) == 0,
              "select monotonic unshare observer timeout clock");
        check(pthread_cond_init(&arg.condition, &condition_attr) == 0,
              "initialize unshare observer condition");
        check(pthread_condattr_destroy(&condition_attr) == 0,
              "destroy unshare observer condition attributes");
        pthread_t worker;
        check(pthread_create(&worker, NULL, observe_shared_state_after_unshare, &arg) == 0,
              "create combined-unshare state observer");

        pthread_mutex_lock(&arg.mutex);
        int observer_ready_error = wait_for_observer_flag(
            &arg.condition, &arg.mutex, &arg.ready);
        pthread_mutex_unlock(&arg.mutex);
        check(observer_ready_error == 0,
              "unshare observer reaches ready phase (wait_error=%d, ready=%d)",
              observer_ready_error, arg.ready);

        struct cap_header header = {.version = 0x20080522, .pid = 0};
        struct cap_data original_caps[2], restricted_caps[2];
        if (expect_success) {
            char deleted_cwd[] = "/tmp/unshare-deleted-cwd-XXXXXX";
            check(mkdtemp(deleted_cwd) != NULL, "create cwd for successful mount unshare");
            check(chdir(deleted_cwd) == 0, "enter cwd before deleting it");
            check(rmdir(deleted_cwd) == 0, "unlink cwd while both threads retain it");
        } else {
            const uint32_t admin_bit = 1U << 21; /* CAP_SYS_ADMIN */
            check(syscall(SYS_capget, &header, original_caps) == 0 &&
                  (original_caps[0].effective & admin_bit), "retain original namespace capability");
            memcpy(restricted_caps, original_caps, sizeof(original_caps));
            restricted_caps[0].effective &= ~admin_bit;
            check(syscall(SYS_capset, &header, restricted_caps) == 0,
                  "remove effective namespace authority without dropping permitted capabilities");
        }

        errno = 0;
        int rc = syscall(SYS_unshare, CLONE_FILES | CLONE_NEWNS | CLONE_NEWUTS);
        int unshare_errno = errno;
        if (!expect_success) {
            check(syscall(SYS_capset, &header, original_caps) == 0, "restore original namespace capability");
            check(rc == -1 && unshare_errno == EPERM,
                  "combined unshare without CAP_SYS_ADMIN fails before publication (rc=%d, errno=%d)",
                  rc, unshare_errno);
        } else {
            check(rc == 0, "combined unshare preserves an already deleted cwd (rc=%d, errno=%d)", rc, unshare_errno);
            char cwd[256];
            errno = 0;
            check(getcwd(cwd, sizeof(cwd)) == NULL && errno == ENOENT,
                  "successful unshare retains the nameless cwd instead of rebinding its pathname");
        }
        check(chdir("/tmp") == 0, "change cwd after combined unshare");

        check(close(fd) == 0, "close fd after combined unshare");
        pthread_mutex_lock(&arg.mutex);
        arg.check_state = 1;
        pthread_cond_signal(&arg.condition);
        pthread_mutex_unlock(&arg.mutex);
        int join_error = pthread_join(worker, NULL);
        check(join_error == 0,
              "join combined-unshare state observer (rc=%d)", join_error);
        check(arg.observer_wait_error == 0,
              "unshare observer receives fd-check release (wait_error=%d, check_state=%d)",
              arg.observer_wait_error, arg.check_state);
        check(arg.fd_state == (expect_success ? 1 : 0) && arg.close_rc == 0,
              "combined unshare publishes the fd table only on success (success=%d, sibling_fd=%d)",
              expect_success, arg.fd_state);
        check(expect_success ? !arg.cwd_ok && arg.cwd_errno == ENOENT
                             : arg.cwd_ok && strcmp(arg.cwd, "/tmp") == 0,
              "combined unshare publishes the FS context only on success (success=%d, sibling_errno=%d)",
              expect_success, arg.cwd_errno);

        ino_t current_inode = current_namespace_inode("uts");
        check(expect_success ? current_inode != original_uts_inode : current_inode == original_uts_inode,
              "combined unshare publishes the UTS namespace only on success");
        current_inode = current_namespace_inode("mnt");
        check(expect_success ? current_inode != original_mnt_inode : current_inode == original_mnt_inode,
              "combined unshare publishes the mount namespace only on success");
        pthread_cond_destroy(&arg.condition);
        pthread_mutex_destroy(&arg.mutex);

        printf("%s\n", expect_success ? "UNSHARE_DELETED_CWD_ISOLATION_PASSED" : "UNSHARE_FAILURE_ROLLBACK_PASSED");
    }
}

static void test_unshare_preserves_pending_pid_namespace(void) {
    pid_t child = fork();
    check(child >= 0, "fork pending PID namespace test child");
    if (child == 0) {
        check(unshare(CLONE_NEWPID) == 0, "prepare child PID namespace");
        check(unshare(CLONE_NEWUTS) == 0,
              "unshare UTS without dropping pending PID namespace");

        pid_t namespace_init = fork();
        check(namespace_init >= 0, "fork into pending PID namespace");
        if (namespace_init == 0)
            _exit(getpid() == 1 ? 0 : 1);

        int status = 0;
        check(waitpid(namespace_init, &status, 0) == namespace_init,
              "wait for pending PID namespace child");
        _exit(WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 1);
    }

    int status = 0;
    check(waitpid(child, &status, 0) == child,
          "wait for pending PID namespace test child");
    check(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "unrelated unshare preserves pending child PID namespace");

    printf("UNSHARE_PENDING_PID_NAMESPACE_PASSED\n");
}

static int clone_child(void *arg) {
    struct clone_arg *a = (struct clone_arg *)arg;

    /* After clone(CLONE_FS), parent and child share the same
       FS_CONTEXT.  chdir here should be visible to the parent. */
    int rc = chdir("/tmp");
    check(rc == 0, "clone child (shared FS) chdir to /tmp (rc=%d, errno=%d)",
          rc, errno);

    /* Signal parent: I've chdir'd. */
    a->shared[0] = 1;
    while (a->shared[1] == 0) usleep(10000);

    /* Parent should have observed /tmp while FS_CONTEXT was shared. */
    while (a->shared[2] == 0) usleep(10000);

    /* Now unshare(CLONE_FS) — break the shared FS_CONTEXT. */
    rc = unshare(CLONE_FS);
    check(rc == 0, "clone child unshare(CLONE_FS) (rc=%d, errno=%d)", rc, errno);

    /* Now cwd changes here must be invisible to parent. */
    rc = chdir("/usr");
    check(rc == 0, "child chdir to /usr after unshare(CLONE_FS) (rc=%d, errno=%d)",
          rc, errno);

    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "child getcwd after unshare");
    check(strcmp(cwd, "/usr") == 0,
          "child cwd is /usr after unshare+chdir: %s", cwd);

    /* Tell parent: child finished its side. */
    a->shared[3] = 1;
    _exit(0);
    return 0;
}

static void test_unshare_fs_basic(void) {
    int rc = unshare(CLONE_FS);
    check(rc == 0, "unshare(CLONE_FS) on independent task (rc=%d, errno=%d)",
          rc, errno);

    rc = unshare(CLONE_FILES);
    check(rc == 0, "unshare(CLONE_FILES) on independent task (rc=%d, errno=%d)",
          rc, errno);

    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "getcwd after unshare(CLONE_FS)");
    check(cwd[0] == '/', "cwd valid after unshare(CLONE_FS): %s", cwd);

    printf("UNSHARE_FS_BASIC_PASSED\n");
}

static void test_clone_fs_unshare_isolation(void) {
    int *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    check(shared != MAP_FAILED, "mmap shared page");
    shared[0] = 0; shared[1] = 0; shared[2] = 0; shared[3] = 0;

    /* Allocate stack on heap for the clone child. */
    char *stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    check(stack != MAP_FAILED, "mmap child stack");
    /* clone(2) takes the *top* of the stack on x86_64. */
    char *stack_top = stack + STACK_SIZE;

    /* Set up parent baseline cwd. */
    mkdir("/tmp/unshare-fs-dirP", 0755);
    int rc = chdir("/tmp/unshare-fs-dirP");
    check(rc == 0, "parent chdir to /tmp/unshare-fs-dirP");

    struct clone_arg arg = { .shared = shared, .barrier = 0 };
    pid_t pid = clone(clone_child, (void *)stack_top,
                      CLONE_FS | CLONE_VM | SIGCHLD, &arg);
    check(pid > 0, "clone(CLONE_FS|CLONE_VM|SIGCHLD) returned pid=%d", pid);

    /* Wait for child to chdir to /tmp (shared FS_CONTEXT). */
    while (shared[0] == 0) usleep(10000);

    /* Verify parent sees child's chdir — proving FS_CONTEXT was shared. */
    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "parent getcwd after child shared chdir");
    check(strcmp(cwd, "/tmp") == 0,
          "parent observes child chdir (shared FS): cwd=%s", cwd);

    /* Tell child to proceed with unshare. */
    shared[1] = 1;
    shared[2] = 1;

    /* Wait for child to unshare(CLONE_FS) + chdir /usr. */
    while (shared[3] == 0) usleep(10000);

    /* Verify parent cwd is still /tmp — isolation works. */
    check(getcwd(cwd, sizeof(cwd)) != NULL, "parent getcwd after child unshare");
    check(strcmp(cwd, "/tmp") == 0,
          "parent cwd unchanged after child unshare+chdir: %s", cwd);

    waitpid(pid, NULL, 0);
    munmap(stack, STACK_SIZE);
    munmap(shared, 4096);
    rmdir("/tmp/unshare-fs-dirP");

    printf("UNSHARE_FS_CLONE_ISOLATION_PASSED\n");
}

static void test_unshare_invalid_flags(void) {
    int rc = unshare(0xDEAD);
    check(rc == -1 && errno == EINVAL,
          "unshare(0xDEAD) returns EINVAL (rc=%d, errno=%d)", rc, errno);

    errno = 0;
    long raw_rc = syscall(SYS_unshare, 1ULL << 32);
    check(raw_rc == -1 && errno == EINVAL,
          "raw unshare high-word unknown flag returns EINVAL (rc=%ld, errno=%d)",
          raw_rc, errno);
    printf("UNSHARE_FS_INVALID_PASSED\n");
}

int main(void) {
    test_thread_unshare_files_isolation();
    test_combined_unshare_state();
    test_unshare_preserves_pending_pid_namespace();
    test_unshare_fs_basic();
    test_clone_fs_unshare_isolation();
    test_unshare_invalid_flags();
    printf("UNSHARE_FS_ALL_PASSED\n");
    return 0;
}
