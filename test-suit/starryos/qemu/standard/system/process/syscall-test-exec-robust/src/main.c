#define _GNU_SOURCE
#include <sched.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#define OWNER_DIED UINT32_C(0x40000000)
#define WAITERS UINT32_C(0x80000000)
#define STACK_SIZE (1024 * 1024)
#define REQUIRE(x) do { if (!(x)) { perror(#x); exit(1); } } while (0)

struct robust_node { struct robust_node *next; };
struct robust_head {
    struct robust_node list;
    long futex_offset;
    struct robust_node *pending;
};
struct shared_lock {
    struct robust_node node;
    uint32_t word;
    uint32_t owner_tid;
};
struct child_args {
    struct shared_lock *lock;
    int ready[2];
    int release[2];
    char ready_fd[24];
    char release_fd[24];
};

static int enter_new_image(void *opaque)
{
    struct child_args *args = opaque;
    close(args->ready[0]);
    close(args->release[1]);
    struct robust_head head = {
        .list = {.next = &args->lock->node},
        .futex_offset = (char *)&args->lock->word - (char *)&args->lock->node,
        .pending = NULL,
    };
    args->lock->node.next = &head.list;
    uint32_t tid = (uint32_t)syscall(SYS_gettid);
    __atomic_store_n(&args->lock->owner_tid, tid, __ATOMIC_RELEASE);
    __atomic_store_n(&args->lock->word, tid | WAITERS, __ATOMIC_RELEASE);
    if (syscall(SYS_set_robust_list, &head, sizeof(head)) != 0)
        return 101;
    char *image_args[] = {"syscall-test-exec-robust", "--after-exec",
                          args->ready_fd, args->release_fd, NULL};
    syscall(SYS_execve, "/proc/self/exe", image_args, environ);
    return 102;
}

static void *exec_worker(void *opaque)
{
    _exit(enter_new_image(opaque));
}

static int exec_from_nonleader(void *opaque)
{
    /* Use pthread creation: the guest libc's public clone wrapper rejects
     * CLONE_THREAD before making a syscall. */
    pthread_t worker;
    int error = pthread_create(&worker, NULL, exec_worker, opaque);
    if (error != 0) {
        fprintf(stderr, "nonleader pthread_create: %s\n", strerror(error));
        return 103;
    }
    for (;;)
        pause();
}

static int check_exec_releases_owner(int flags, int nonleader)
{
    struct child_args args;
    args.lock = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    REQUIRE(args.lock != MAP_FAILED);
    void *stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(stack != MAP_FAILED);
    REQUIRE(pipe(args.ready) == 0);
    REQUIRE(pipe(args.release) == 0);
    snprintf(args.ready_fd, sizeof(args.ready_fd), "%d", args.ready[1]);
    snprintf(args.release_fd, sizeof(args.release_fd), "%d", args.release[0]);
    pid_t child = clone(nonleader ? exec_from_nonleader : enter_new_image,
                        (char *)stack + STACK_SIZE,
                        flags | SIGCHLD, &args);
    REQUIRE(child > 0);
    close(args.ready[1]);
    close(args.release[0]);
    char byte;
    if (read(args.ready[0], &byte, 1) != 1) {
        int status = 0;
        pid_t waited = waitpid(child, &status, 0);
        fprintf(stderr, "exec image did not report readiness: child=%d waited=%d status=%#x nonleader=%d\n",
                child, waited, status, nonleader);
        exit(1);
    }
    /* The new image stays alive until this observation finishes. A later
     * process exit therefore cannot conceal a missing exec-time cleanup. */
    uint32_t observed = __atomic_load_n(&args.lock->word, __ATOMIC_ACQUIRE);
    uint32_t owner = __atomic_load_n(&args.lock->owner_tid, __ATOMIC_ACQUIRE);
    /* Linux de_thread changes the caller's TID before futex_exec_release.
     * A word containing the former nonleader TID no longer matches the
     * caller, so that case must not be changed by the robust walk. */
    uint32_t expected = nonleader ? owner | WAITERS : OWNER_DIED | WAITERS;
    int valid = observed == expected && (!nonleader || owner != (uint32_t)child);
    REQUIRE(write(args.release[1], "x", 1) == 1);
    int status;
    REQUIRE(waitpid(child, &status, 0) == child);
    valid = valid && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    close(args.ready[0]);
    close(args.release[1]);
    munmap(stack, STACK_SIZE);
    munmap(args.lock, 4096);
    printf("exec robust %s: word=%#x %s\n",
           nonleader ? "nonleader" : (flags & CLONE_VM ? "shared-mm" : "private-mm"),
           observed, valid ? "PASS" : "FAIL");
    return !valid;
}

int main(int argc, char **argv)
{
    if (argc == 4 && strcmp(argv[1], "--after-exec") == 0) {
        int ready = atoi(argv[2]);
        int release = atoi(argv[3]);
        char byte;
        REQUIRE(write(ready, "x", 1) == 1);
        REQUIRE(read(release, &byte, 1) == 1);
        return 0;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    alarm(30);
    int failures = check_exec_releases_owner(0, 0);
    failures += check_exec_releases_owner(CLONE_VM | CLONE_VFORK, 0);
    failures += check_exec_releases_owner(0, 1);
    alarm(0);
    return failures != 0;
}
