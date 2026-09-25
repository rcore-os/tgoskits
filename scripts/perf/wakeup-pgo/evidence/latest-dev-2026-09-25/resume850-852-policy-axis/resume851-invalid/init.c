#include <errno.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_case(int round, int other)
{
    const char *mode = other ? "other" : "fifo";
    printf("RESUME851_LINUX_START round=%d mode=%s\n", round, mode);
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char *const argv[] = {"/wake-cost", other ? "other" : "fifo", NULL};
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
    printf("RESUME851_LINUX_DONE round=%d mode=%s exit=%d\n", round, mode, result);
    return result;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("RESUME851_LINUX_INIT_START");
    int failures = run_case(1, 0) != 0;
    failures += run_case(2, 1) != 0;
    failures += run_case(3, 1) != 0;
    failures += run_case(4, 0) != 0;
    failures += run_case(5, 0) != 0;
    failures += run_case(6, 1) != 0;
    printf("RESUME851_LINUX_INIT_DONE failures=%d\n", failures);
    for (;;) {
        pause();
    }
}
