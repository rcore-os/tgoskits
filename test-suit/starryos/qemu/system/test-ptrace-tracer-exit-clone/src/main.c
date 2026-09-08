#define _GNU_SOURCE
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>

static void *worker(void *arg)
{
    (void)arg;
    for (;;) {
        pause();
    }
    return NULL;
}

int main(void)
{
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0 || raise(SIGSTOP) != 0) {
            _exit(101);
        }
        pthread_t thread;
        if (pthread_create(&thread, NULL, worker, NULL) != 0) {
            _exit(102);
        }
        worker(NULL);
        _exit(103);
    }
    int status = 0;
    if (waitpid(child, &status, __WALL) != child || !WIFSTOPPED(status)
        || WSTOPSIG(status) != SIGSTOP) {
        fprintf(stderr, "FAIL: initial stop status=%#x\n", status);
        return 1;
    }
    if (ptrace(PTRACE_SETOPTIONS, child, NULL, (void *)PTRACE_O_TRACECLONE) != 0
        || ptrace(PTRACE_CONT, child, NULL, NULL) != 0) {
        perror("enable clone tracing");
        return 1;
    }
    siginfo_t info = {0};
    if (waitid(P_PID, child, &info, WSTOPPED | WNOWAIT | __WALL) != 0
        || info.si_pid != child || info.si_code != CLD_TRAPPED
        || info.si_status != (SIGTRAP | (PTRACE_EVENT_CLONE << 8))) {
        fprintf(stderr, "FAIL: clone stop pid=%ld status=%#x\n",
                (long)info.si_pid, info.si_status);
        return 1;
    }
    /* Leave both stops unresumed. Tracer exit must detach them, and the
     * system runner must finish namespace cleanup before reporting success. */
    puts("DONE: tracer exits with parent and new thread stopped");
    return 0;
}
