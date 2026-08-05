// Migrated from the former nix-sandbox-debug suite.
#define _GNU_SOURCE

/*
 * Verify the clone and clone3 ABI used while starting a Nix sandbox builder.
 * This covers CLONE_PARENT namespace setup as well as the validation rules
 * that differ between legacy clone and clone3.
 */

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_clone3
#define __NR_clone3 435
#endif

#ifndef __WALL
#define __WALL 0x40000000
#endif

#define THREAD_STACK_SIZE (64 * 1024)

struct clone3_args {
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

static void fail(const char *stage)
{
    printf("TEST_NIX_CLONE_PARENT_FAILED stage=%s errno=%d (%s)\n", stage,
           errno, strerror(errno));
    exit(1);
}

static void timeout_handler(int signo)
{
    (void)signo;
    static const char marker[] =
        "TEST_NIX_CLONE_PARENT_FAILED stage=timeout\n";
    ssize_t ignored = write(STDOUT_FILENO, marker, sizeof(marker) - 1);
    (void)ignored;
    _exit(1);
}

static void write_all(int fd, const void *buffer, size_t length,
                      const char *stage)
{
    const char *cursor = buffer;
    while (length > 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            fail(stage);
        }
        cursor += written;
        length -= (size_t)written;
    }
}

static void read_all(int fd, void *buffer, size_t length, const char *stage)
{
    char *cursor = buffer;
    while (length > 0) {
        ssize_t count = read(fd, cursor, length);
        if (count < 0) {
            if (errno == EINTR)
                continue;
            fail(stage);
        }
        if (count == 0) {
            errno = EPIPE;
            fail(stage);
        }
        cursor += count;
        length -= (size_t)count;
    }
}

static void write_proc_file(pid_t pid, const char *name, const char *value)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "/proc/%d/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail(name);
    }

    int fd = open(path, O_WRONLY);
    if (fd < 0)
        fail(name);
    write_all(fd, value, strlen(value), name);
    if (close(fd) < 0)
        fail(name);
}

static void open_namespace(pid_t pid, const char *name)
{
    char path[128];
    int length = snprintf(path, sizeof(path), "/proc/%d/ns/%s", pid, name);
    if (length < 0 || (size_t)length >= sizeof(path)) {
        errno = ENAMETOOLONG;
        fail(name);
    }

    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        fail(name);
    if (close(fd) < 0)
        fail(name);
}

static const char thread_done = '1';

struct legacy_namespace_child {
    int sync_read;
    int sync_write;
    int stage_write;
};

static int legacy_namespace_child_main(void *opaque)
{
    struct legacy_namespace_child *child = opaque;
    close(child->sync_write);
    write_all(child->stage_write, "C", 1, "legacy-namespace-child-entered");

    char token;
    read_all(child->sync_read, &token, 1, "legacy-namespace-child-sync");
    if (token != '1') {
        errno = EPROTO;
        fail("legacy-namespace-child-token");
    }
    write_all(child->stage_write, "R", 1,
              "legacy-namespace-child-released");
    return 0;
}

static pid_t raw_clone3(struct clone3_args *args, int notify_fd)
{
    long result;

    __asm__ volatile("mov %[notify_fd], %%r12d\n\t"
                     "mov %[done], %%r13\n\t"
                     "syscall\n\t"
                     "test %%rax, %%rax\n\t"
                     "jnz 1f\n\t"
                     "mov %[write_nr], %%eax\n\t"
                     "mov %%r12d, %%edi\n\t"
                     "mov %%r13, %%rsi\n\t"
                     "mov $1, %%edx\n\t"
                     "syscall\n\t"
                     "mov %[exit_nr], %%eax\n\t"
                     "xor %%edi, %%edi\n\t"
                     "syscall\n\t"
                     "ud2\n"
                     "1:"
                     : "=a"(result)
                     : "0"((long)__NR_clone3), "D"(args),
                       "S"(sizeof(*args)), [notify_fd] "r"(notify_fd),
                       [done] "r"(&thread_done),
                       [write_nr] "i"(__NR_write),
                       [exit_nr] "i"(__NR_exit)
                     : "rcx", "r11", "r12", "r13", "memory");

    return (pid_t)result;
}

static pid_t raw_clone(unsigned long flags, void *child_stack, int notify_fd)
{
    long result;
    register unsigned long child_tid __asm__("r10") = 0;
    register unsigned long tls __asm__("r8") = 0;

    __asm__ volatile("mov %[notify_fd], %%r12d\n\t"
                     "mov %[done], %%r13\n\t"
                     "syscall\n\t"
                     "test %%rax, %%rax\n\t"
                     "jnz 1f\n\t"
                     "mov %[write_nr], %%eax\n\t"
                     "mov %%r12d, %%edi\n\t"
                     "mov %%r13, %%rsi\n\t"
                     "mov $1, %%edx\n\t"
                     "syscall\n\t"
                     "mov %[exit_nr], %%eax\n\t"
                     "xor %%edi, %%edi\n\t"
                     "syscall\n\t"
                     "ud2\n"
                     "1:"
                     : "=a"(result)
                     : "0"((long)__NR_clone), "D"(flags), "S"(child_stack),
                       "d"(0UL), "r"(child_tid), "r"(tls),
                       [notify_fd] "r"(notify_fd),
                       [done] "r"(&thread_done),
                       [write_nr] "i"(__NR_write),
                       [exit_nr] "i"(__NR_exit)
                     : "rcx", "r11", "r12", "r13", "memory");

    return (pid_t)result;
}

static void check_clone3_parent_exit_signal(void)
{
    struct clone3_args args = {
        .flags = CLONE_PARENT,
        .exit_signal = SIGCHLD,
    };

    errno = 0;
    if (syscall(__NR_clone3, &args, sizeof(args)) != -1 || errno != EINVAL) {
        errno = EPROTO;
        fail("clone3-parent-exit-signal");
    }
}

static void check_legacy_clone_parent_exit_signal(void)
{
    int pid_pipe[2];
    if (pipe(pid_pipe) < 0)
        fail("legacy-clone-pipe");

    pid_t helper = fork();
    if (helper < 0)
        fail("legacy-clone-helper");

    if (helper == 0) {
        close(pid_pipe[0]);
        pid_t child = (pid_t)syscall(__NR_clone, CLONE_PARENT | SIGCHLD,
                                    NULL, NULL, NULL, 0UL);
        if (child < 0)
            fail("legacy-clone-parent-exit-signal");
        if (child == 0)
            _exit(0);
        write_all(pid_pipe[1], &child, sizeof(child), "legacy-clone-pid");
        _exit(0);
    }

    close(pid_pipe[1]);
    pid_t child;
    read_all(pid_pipe[0], &child, sizeof(child), "legacy-clone-read-pid");
    close(pid_pipe[0]);

    int status;
    if (waitpid(helper, &status, 0) != helper || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("legacy-clone-helper-status");
    }
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("legacy-clone-child-status");
    }
}

static void check_legacy_clone_namespace_sync(void)
{
    int sync_pipe[2];
    int pid_pipe[2];
    int stage_pipe[2];
    if (pipe(sync_pipe) < 0 || pipe(pid_pipe) < 0 || pipe(stage_pipe) < 0)
        fail("legacy-namespace-pipes");

    pid_t helper = fork();
    if (helper < 0)
        fail("legacy-namespace-helper");
    if (helper == 0) {
        close(sync_pipe[1]);
        close(pid_pipe[0]);
        close(stage_pipe[0]);

        void *stack = mmap(NULL, THREAD_STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stack == MAP_FAILED)
            fail("legacy-namespace-stack");
        struct legacy_namespace_child child = {
            .sync_read = sync_pipe[0],
            .sync_write = sync_pipe[1],
            .stage_write = stage_pipe[1],
        };
        int flags = CLONE_PARENT | CLONE_NEWPID | CLONE_NEWUSER | CLONE_NEWNS |
                    CLONE_NEWIPC | CLONE_NEWUTS | CLONE_NEWNET | SIGCHLD;
        pid_t builder = clone(legacy_namespace_child_main,
                              (char *)stack + THREAD_STACK_SIZE, flags, &child);
        if (builder < 0)
            fail("legacy-namespace-clone");
        write_all(pid_pipe[1], &builder, sizeof(builder),
                  "legacy-namespace-send-pid");
        _exit(0);
    }

    close(sync_pipe[0]);
    close(pid_pipe[1]);
    close(stage_pipe[1]);

    int status;
    if (waitpid(helper, &status, 0) != helper || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("legacy-namespace-helper-status");
    }

    pid_t builder;
    read_all(pid_pipe[0], &builder, sizeof(builder),
             "legacy-namespace-receive-pid");
    char stage;
    read_all(stage_pipe[0], &stage, 1, "legacy-namespace-child-entered");
    if (stage != 'C') {
        errno = EPROTO;
        fail("legacy-namespace-child-entered-token");
    }

    char mapping[64];
    int length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getuid());
    if (length < 0 || (size_t)length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("legacy-namespace-uid-mapping");
    }
    write_proc_file(builder, "uid_map", mapping);
    write_proc_file(builder, "setgroups", "deny\n");
    length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getgid());
    if (length < 0 || (size_t)length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("legacy-namespace-gid-mapping");
    }
    write_proc_file(builder, "gid_map", mapping);
    open_namespace(builder, "mnt");
    open_namespace(builder, "user");
    write_all(sync_pipe[1], "1", 1, "legacy-namespace-parent-sync");

    read_all(stage_pipe[0], &stage, 1, "legacy-namespace-child-released");
    if (stage != 'R') {
        errno = EPROTO;
        fail("legacy-namespace-child-released-token");
    }
    if (waitpid(builder, &status, 0) != builder || !WIFEXITED(status) ||
        WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        fail("legacy-namespace-builder-status");
    }

    close(sync_pipe[1]);
    close(pid_pipe[0]);
    close(stage_pipe[0]);
    puts("TEST_NIX_CLONE_PARENT_STAGE legacy_namespace_sync=passed");
}

static void check_thread_parent_combinations(void)
{
    void *clone_stack = mmap(NULL, THREAD_STACK_SIZE, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    void *clone3_stack = mmap(NULL, THREAD_STACK_SIZE, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (clone_stack == MAP_FAILED || clone3_stack == MAP_FAILED)
        fail("mmap-thread-stacks");

    int notify_pipe[2];
    if (pipe(notify_pipe) < 0)
        fail("pipe-thread-notify");

    unsigned long thread_flags =
        CLONE_VM | CLONE_SIGHAND | CLONE_THREAD | CLONE_PARENT;
    void *stack_top = (char *)clone_stack + THREAD_STACK_SIZE;

    pid_t tid = raw_clone(thread_flags, stack_top, notify_pipe[1]);
    if (tid < 0)
        fail("clone-thread-parent");
    char token;
    read_all(notify_pipe[0], &token, 1, "clone-thread-notify");

    struct clone3_args args = {
        .flags = thread_flags,
        .stack = (unsigned long long)clone3_stack,
        .stack_size = THREAD_STACK_SIZE,
    };
    tid = raw_clone3(&args, notify_pipe[1]);
    if (tid < 0)
        fail("clone3-thread-parent");
    read_all(notify_pipe[0], &token, 1, "clone3-thread-notify");

    close(notify_pipe[0]);
    close(notify_pipe[1]);
    if (munmap(clone_stack, THREAD_STACK_SIZE) < 0 ||
        munmap(clone3_stack, THREAD_STACK_SIZE) < 0)
        fail("munmap-thread-stacks");
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, timeout_handler);
    alarm(20);

    check_clone3_parent_exit_signal();
    check_legacy_clone_parent_exit_signal();
    check_legacy_clone_namespace_sync();
    check_thread_parent_combinations();

    int sync_pipe[2];
    int pid_pipe[2];
    if (pipe(sync_pipe) < 0 || pipe(pid_pipe) < 0)
        fail("pipe");

    pid_t helper = fork();
    if (helper < 0)
        fail("fork-helper");

    if (helper == 0) {
        close(sync_pipe[1]);
        close(pid_pipe[0]);

        struct clone3_args args = {
            .flags = CLONE_PARENT | CLONE_NEWPID | CLONE_NEWUSER |
                     CLONE_NEWNS | CLONE_NEWIPC | CLONE_NEWUTS |
                     CLONE_NEWNET,
            .exit_signal = 0,
        };
        pid_t child = (pid_t)syscall(__NR_clone3, &args, sizeof(args));
        if (child < 0)
            fail("clone3");
        if (child == 0) {
            close(pid_pipe[1]);
            char token;
            read_all(sync_pipe[0], &token, 1, "child-sync");
            _exit(token == '1' ? 0 : 2);
        }

        write_all(pid_pipe[1], &child, sizeof(child), "send-pid");
        _exit(0);
    }

    close(sync_pipe[0]);
    close(pid_pipe[1]);

    int helper_status;
    if (waitpid(helper, &helper_status, 0) != helper)
        fail("wait-helper");
    if (!WIFEXITED(helper_status) || WEXITSTATUS(helper_status) != 0) {
        errno = ECHILD;
        fail("helper-status");
    }

    pid_t child;
    read_all(pid_pipe[0], &child, sizeof(child), "receive-pid");
    printf("TEST_NIX_CLONE_PARENT_STAGE child=%d\n", child);

    char mapping[64];
    int mapping_length =
        snprintf(mapping, sizeof(mapping), "0 %u 1\n", getuid());
    if (mapping_length < 0 || (size_t)mapping_length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("uid-mapping");
    }
    write_proc_file(child, "uid_map", mapping);
    write_proc_file(child, "setgroups", "deny\n");
    mapping_length = snprintf(mapping, sizeof(mapping), "0 %u 1\n", getgid());
    if (mapping_length < 0 || (size_t)mapping_length >= sizeof(mapping)) {
        errno = EOVERFLOW;
        fail("gid-mapping");
    }
    write_proc_file(child, "gid_map", mapping);
    open_namespace(child, "mnt");
    open_namespace(child, "user");
    write_all(sync_pipe[1], "1", 1, "parent-sync");

    int child_status;
    if (waitpid(child, &child_status, __WALL) != child)
        fail("wait-child");
    if (!WIFEXITED(child_status) || WEXITSTATUS(child_status) != 0) {
        errno = ECHILD;
        fail("child-status");
    }

    alarm(0);
    puts("TEST_NIX_CLONE_PARENT_PASSED");
    return 0;
}
