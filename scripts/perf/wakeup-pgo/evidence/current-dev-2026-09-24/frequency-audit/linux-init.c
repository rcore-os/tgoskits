#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_probe(const char *cpu)
{
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char *const argv[] = {"/probe", (char *)cpu, NULL};
        execv("/probe", argv);
        perror("execv");
        _exit(127);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        perror("waitpid");
        return 1;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("RESUME749_INIT_START\n");
    int cpu0 = run_probe("0");
    int cpu1 = run_probe("1");
    printf("RESUME749_INIT_DONE cpu0=%d cpu1=%d\n", cpu0, cpu1);
    for (;;) {
        pause();
    }
}
