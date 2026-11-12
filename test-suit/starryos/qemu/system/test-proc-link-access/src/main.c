#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_openat2
#define SYS_openat2 437
#endif

/* Use the raw ABI: the minimal guest sysroot need not ship Linux UAPI headers. */
struct raw_open_how {
    uint64_t flags, mode, resolve;
};
struct cap_header {
    uint32_t version;
    int pid;
};
struct cap_data {
    uint32_t effective, permitted, inheritable;
};

struct probe {
    pid_t pid;
    int fd;
    struct stat expected[5];
    int result;
};

static int diagnostic(const char *stage, const char *path)
{
    fprintf(stderr, "FAIL: %s path=%s errno=%d (%s)\n",
            stage, path, errno, strerror(errno));
    return 1;
}

static int clear_ptrace_capability(void)
{
    struct cap_header header = {.version = 0x20080522, .pid = 0};
    struct cap_data caps[2];
    const uint32_t ptrace_bit = 1U << 19; /* CAP_SYS_PTRACE */
    if (syscall(SYS_capget, &header, caps) != 0) return -1;
    caps[0].effective &= ~ptrace_bit;
    caps[0].permitted &= ~ptrace_bit;
    caps[0].inheritable &= ~ptrace_bit;
    if (syscall(SYS_capset, &header, caps) != 0 ||
        syscall(SYS_capget, &header, caps) != 0) return -1;
    printf("INFO: uid=%u gid=%u CapEff=%08x%08x CapPrm=%08x%08x\n",
           getuid(), getgid(), caps[1].effective, caps[0].effective,
           caps[1].permitted, caps[0].permitted);
    if ((caps[0].effective | caps[0].permitted) & ptrace_bit) {
        errno = EPERM;
        return -1;
    }
    return 0;
}

static int same_object(const struct stat *a, const struct stat *b)
{
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino &&
           (a->st_mode & S_IFMT) == (b->st_mode & S_IFMT);
}

static int probe_links(const struct probe *p, int allowed)
{
    char fd_name[32], path[96], text[512];
    snprintf(fd_name, sizeof(fd_name), "fd/%d", p->fd);
    const char *names[] = {fd_name, "cwd", "root", "exe", "ns/mnt"};
    for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); ++i) {
        snprintf(path, sizeof(path), "/proc/%d/%s", p->pid, names[i]);
        errno = 0;
        ssize_t n = syscall(SYS_readlinkat, AT_FDCWD, path, text, sizeof(text));
        if (allowed ? n <= 0 : (n != -1 || errno != EACCES))
            return diagnostic("readlinkat access decision", path);

        /* A nofollow observation must not authorize or acquire the target. */
        struct stat link, pinned, target;
        if (fstatat(AT_FDCWD, path, &link, AT_SYMLINK_NOFOLLOW) != 0 ||
            !S_ISLNK(link.st_mode))
            return diagnostic("lstat observes the link even when denied", path);
        int fd = syscall(SYS_openat, AT_FDCWD, path,
                         O_PATH | O_NOFOLLOW | O_CLOEXEC, 0);
        int bad = fd < 0 || fstat(fd, &pinned) != 0 ||
                  !S_ISLNK(pinned.st_mode) || !same_object(&link, &pinned);
        if (fd >= 0) close(fd);
        if (bad) return diagnostic("O_PATH nofollow observes only the link", path);

        errno = 0;
        int ret = fstatat(AT_FDCWD, path, &target, 0);
        if (allowed ? (ret != 0 || !same_object(&p->expected[i], &target))
                    : (ret != -1 || errno != EACCES))
            return diagnostic("stat target identity/access decision", path);
        struct raw_open_how how = {.flags = O_PATH | O_CLOEXEC};
        for (int use_openat2 = 0; use_openat2 < 2; ++use_openat2) {
            errno = 0;
            fd = use_openat2
                ? syscall(SYS_openat2, AT_FDCWD, path, &how, sizeof(how))
                : syscall(SYS_openat, AT_FDCWD, path, O_PATH | O_CLOEXEC, 0);
            int saved = errno;
            bad = allowed
                ? (fd < 0 || fstat(fd, &target) != 0 ||
                   !same_object(&p->expected[i], &target))
                : (fd != -1 || saved != EACCES);
            if (fd >= 0) close(fd);
            if (bad) {
                errno = saved;
                return diagnostic(use_openat2 ? "openat2 access/identity"
                                             : "openat access/identity", path);
            }
        }
    }
    return 0;
}

static void *thread_probe(void *opaque)
{
    struct probe *p = opaque;
    printf("INFO: same-group caller tid=%ld target tgid=%d dumpable=0\n",
           syscall(SYS_gettid), p->pid);
    p->result = probe_links(p, 1);
    return NULL;
}

static int token(int fd, char *value, int writing)
{
    ssize_t n;
    do {
        n = writing ? write(fd, value, 1) : read(fd, value, 1);
    } while (n < 0 && errno == EINTR);
    return n == 1 ? 0 : -1;
}

static int target_process(int commands, int events, struct probe *p)
{
    p->pid = getpid();
    char value = 'R';
    if (syscall(SYS_prctl, PR_SET_DUMPABLE, 1, 0, 0, 0) != 0 ||
        token(events, &value, 1) != 0) return 2;
    for (;;) {
        ssize_t n;
        do {
            n = read(commands, &value, 1);
        } while (n < 0 && errno == EINTR);
        if (n == 0) return 0;
        if (n != 1 || (value != '0' && value != '1')) return 2;
        int dumpable = value == '1';
        if (syscall(SYS_prctl, PR_SET_DUMPABLE, dumpable, 0, 0, 0) != 0)
            return 2;
        /* Self introspection is allowed even when this task is nondumpable. */
        if (probe_links(p, 1) != 0) return 1;
        value = dumpable ? 'R' : 'D';
        if (token(events, &value, 1) != 0) return 2;
    }
}

static int observe_target(const struct probe *p, int commands, int events)
{
    printf("INFO: sibling observer pid=%d target pid=%d parent pid=%d\n",
           getpid(), p->pid, getppid());
    char value;
    if (token(events, &value, 0) != 0 || value != 'R') {
        diagnostic("SETUP target ready", "");
        return 2;
    }
    /* READ_FSCREDS is not ptrace attach: the observer and target are siblings.
     * IDs and capability sets match; CAP_SYS_PTRACE is absent in both tasks. */
    if (probe_links(p, 1) != 0) return 1;
    value = '0';
    if (token(commands, &value, 1) != 0 ||
        token(events, &value, 0) != 0 || value != 'D')
        return diagnostic("target nondumpable handshake", "");
    if (probe_links(p, 0) != 0) return 1;
    value = '1';
    if (token(commands, &value, 1) != 0 ||
        token(events, &value, 0) != 0 || value != 'R')
        return diagnostic("target dumpability restored handshake", "");
    if (probe_links(p, 1) != 0) return 1;
    puts("INFO: cross-task allow -> EACCES -> allow; nofollow stays link-only");
    return 0;
}

int main(void)
{
    int failed = 2, commands[2] = {-1, -1}, events[2] = {-1, -1};
    int fixture = -1;
    pid_t child = -1, observer = -1;
    char name[] = "/tmp/proc-link-access-XXXXXX";
    struct probe p = {0};
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGPIPE, SIG_IGN);
    puts("STARRY_SYSTEM_TEST_BEGIN: test-proc-link-access");
    if (clear_ptrace_capability() != 0) {
        diagnostic("SETUP clear and verify CAP_SYS_PTRACE", "");
        goto cleanup;
    }
    fixture = mkstemp(name);
    if (fixture < 0 || unlink(name) != 0) {
        diagnostic("SETUP unlinked file fixture", name);
        goto cleanup;
    }
    p.pid = getpid();
    p.fd = fixture;
    if (fstat(fixture, &p.expected[0]) != 0 ||
        fstatat(AT_FDCWD, ".", &p.expected[1], 0) != 0 ||
        fstatat(AT_FDCWD, "/", &p.expected[2], 0) != 0 ||
        fstatat(AT_FDCWD, "/proc/self/exe", &p.expected[3], 0) != 0 ||
        fstatat(AT_FDCWD, "/proc/self/ns/mnt", &p.expected[4], 0) != 0 ||
        syscall(SYS_prctl, PR_SET_DUMPABLE, 0, 0, 0, 0) != 0) {
        diagnostic("SETUP expected objects and nondumpable thread group", "");
        goto cleanup;
    }
    pthread_t thread;
    int error = pthread_create(&thread, NULL, thread_probe, &p);
    if (error != 0) {
        errno = error;
        diagnostic("SETUP sibling thread", "");
        goto cleanup;
    }
    error = pthread_join(thread, NULL);
    if (error != 0) {
        errno = error;
        diagnostic("SETUP join sibling thread", "");
        goto cleanup;
    }
    if (p.result != 0) {
        failed = 1;
        goto cleanup;
    }
    if (syscall(SYS_prctl, PR_SET_DUMPABLE, 1, 0, 0, 0) != 0 ||
        pipe(commands) != 0 || pipe(events) != 0) {
        diagnostic("SETUP dumpability and event pipes", "");
        goto cleanup;
    }
    child = fork();
    if (child < 0) {
        diagnostic("SETUP fork target", "");
        goto cleanup;
    }
    if (child == 0) {
        close(commands[1]);
        close(events[0]);
        int result = target_process(commands[0], events[1], &p);
        close(commands[0]);
        close(events[1]);
        close(fixture);
        _exit(result);
    }
    close(commands[0]);
    commands[0] = -1;
    close(events[1]);
    events[1] = -1;
    p.pid = child;
    observer = fork();
    if (observer < 0) {
        diagnostic("SETUP fork sibling observer", "");
        goto cleanup;
    }
    if (observer == 0) {
        int result = observe_target(&p, commands[1], events[0]);
        close(commands[1]);
        close(events[0]);
        close(fixture);
        _exit(result);
    }
    failed = 0;
cleanup:
    /* Only the observer holds the remaining command writer. EOF releases the
     * target both after the last probe and on observer/coordinator failure. */
    for (int i = 0; i < 2; ++i) {
        if (commands[i] >= 0) close(commands[i]);
        if (events[i] >= 0) close(events[i]);
    }
    if (observer > 0) {
        int status = 0;
        if (waitpid(observer, &status, 0) != observer ||
            !WIFEXITED(status) || WEXITSTATUS(status) != 0)
            failed |= diagnostic("reap sibling observer", "");
    }
    if (child > 0) {
        int status = 0;
        if (waitpid(child, &status, 0) != child ||
            !WIFEXITED(status) || WEXITSTATUS(status) != 0)
            failed |= diagnostic("reap target", "");
    }
    if (fixture >= 0) close(fixture);
    if (unlink(name) != 0 && errno != ENOENT)
        failed |= diagnostic("clean fixture", name);
    if (failed == 0) puts("TEST_PROC_LINK_ACCESS_PASSED");
    return failed;
}
