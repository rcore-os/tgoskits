#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

__attribute__((noinline)) static void crash_leaf(void)
{
    volatile int *p = (volatile int *)0;
    *p = 42;
}

__attribute__((noinline)) static void crash_level_3(void)
{
    crash_leaf();
}

__attribute__((noinline)) static void crash_level_2(void)
{
    crash_level_3();
}

__attribute__((noinline)) static void crash_level_1(void)
{
    crash_level_2();
}

int main(void)
{
    pid_t pid = fork();
    if (pid < 0) {
        puts("FAIL: fork");
        return 1;
    }

    if (pid == 0) {
        crash_level_1();
        _exit(0);
    }

    int status = 0;
    pid_t got = waitpid(pid, &status, 0);
    if (got != pid) {
        puts("FAIL: waitpid");
        return 1;
    }

    int by_signal = WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV;
    int by_exit = WIFEXITED(status)
                  && (WEXITSTATUS(status) == SIGSEGV
                      || WEXITSTATUS(status) == 128 + SIGSEGV);
    if (!by_signal && !by_exit) {
        printf("FAIL: child status %#x\n", status);
        return 1;
    }

    puts("DONE: 1 pass, 0 fail");
    return 0;
}
