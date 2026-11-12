#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* Isolate thread publication/de_thread from the separate NULL argv ABI probe. */
static void *replace_from_nonleader(void *arg)
{
    char *const argv[] = {"test-thread-lifecycle-exec", "after", arg, NULL};
    char *const envp[] = {NULL};
    if ((pid_t)syscall(SYS_gettid) == getpid())
        _exit(21);
    syscall(SYS_execve, "/proc/self/exe", argv, envp);
    _exit(22);
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "after") == 0) {
        pid_t original_tgid = (pid_t)strtol(argv[2], NULL, 10);
        if (getpid() != original_tgid || (pid_t)syscall(SYS_gettid) != original_tgid)
            return 23;
        return 0;
    }
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char tgid[32];
        snprintf(tgid, sizeof(tgid), "%ld", (long)getpid());
        pthread_t worker;
        if (pthread_create(&worker, NULL, replace_from_nonleader, tgid) != 0)
            _exit(24);
        /* exec must retire this leader; an unexpectedly returning worker fails. */
        pthread_join(worker, NULL);
        _exit(25);
    }
    int status;
    pid_t waited;
    do { waited = waitpid(child, &status, 0); } while (waited < 0 && errno == EINTR);
    if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "thread lifecycle exec failed: waited=%ld status=%d\n", (long)waited, waited < 0 ? -1 : status);
        return 1;
    }
    puts("thread lifecycle nonleader exec and wait passed");
    return 0;
}
