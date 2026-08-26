/*
 * Verify that execve can reap a sibling blocked in the vfork completion
 * future. The OS publishes an exit request and a sticky interruption before
 * waking the scheduler thread. Without the interruption, a direct wake can be
 * consumed before LocalExecutor commits to park and execve waits forever.
 */

#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#define SELF_PATH \
    "/usr/bin/starry-test-suit/bugfix-exec-sibling-vfork-wake"

static atomic_int blocker_started;

static void timeout_handler(int signo)
{
    static const char message[] =
        "TEST FAILED: execve did not reap the vfork-blocked sibling\n";

    (void)signo;
    (void)write(STDERR_FILENO, message, sizeof(message) - 1);
    _exit(124);
}

static void *vfork_blocker(void *unused)
{
    (void)unused;
    atomic_store_explicit(&blocker_started, 1, memory_order_release);

    pid_t child = vfork();
    if (child == 0) {
        for (;;) {
            pause();
        }
    }
    if (child < 0) {
        perror("vfork");
        _exit(3);
    }

    /*
     * A successful exec in the sibling interrupts this wait and the kernel
     * consumes its exit request before this thread can continue in userspace.
     */
    return NULL;
}

static void *exec_from_nonleader(void *unused)
{
    (void)unused;
    char *const argv[] = { (char *)SELF_PATH, (char *)"post-exec", NULL };
    char *const envp[] = { NULL };

    execve(SELF_PATH, argv, envp);
    perror("execve");
    _exit(2);
}

static int post_exec(void)
{
    alarm(0);
    pid_t pid = getpid();
    pid_t tid = (pid_t)syscall(SYS_gettid);
    if (tid != pid) {
        fprintf(stderr, "TEST FAILED: post-exec tid=%d pid=%d\n", tid, pid);
        return EXIT_FAILURE;
    }

    puts("TEST PASSED: exec reaped vfork-blocked sibling");
    return EXIT_SUCCESS;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    if (argc == 2 && strcmp(argv[1], "post-exec") == 0) {
        return post_exec();
    }

    struct sigaction action = {
        .sa_handler = timeout_handler,
    };
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, NULL) != 0) {
        perror("sigaction");
        return EXIT_FAILURE;
    }
    alarm(10);

    pthread_t blocker;
    int error = pthread_create(&blocker, NULL, vfork_blocker, NULL);
    if (error != 0) {
        fprintf(stderr, "pthread_create blocker: %s\n", strerror(error));
        return EXIT_FAILURE;
    }

    while (atomic_load_explicit(&blocker_started, memory_order_acquire) == 0) {
        sched_yield();
    }
    usleep(50000);

    pthread_t executor;
    error = pthread_create(&executor, NULL, exec_from_nonleader, NULL);
    if (error != 0) {
        fprintf(stderr, "pthread_create executor: %s\n", strerror(error));
        return EXIT_FAILURE;
    }

    for (;;) {
        pause();
    }
}
