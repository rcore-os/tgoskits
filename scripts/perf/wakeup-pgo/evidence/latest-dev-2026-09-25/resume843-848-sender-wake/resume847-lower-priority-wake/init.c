#include <errno.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_case(int round)
{
    printf("RESUME847_LINUX_START round=%d\n", round);
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char *const argv[] = {"/wake-cost", NULL};
        execv(argv[0], argv);
        perror("execv wake-cost");
        _exit(127);
    }
    int status;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) {
            perror("waitpid");
            return 1;
        }
    }
    int result = WIFEXITED(status) ? WEXITSTATUS(status) : 128;
    printf("RESUME847_LINUX_DONE round=%d exit=%d\n", round, result);
    return result;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("RESUME847_LINUX_INIT_START");
    int failures = run_case(1) != 0;
    failures += run_case(2) != 0;
    printf("RESUME847_LINUX_INIT_DONE failures=%d\n", failures);
    for (;;) {
        pause();
    }
}
