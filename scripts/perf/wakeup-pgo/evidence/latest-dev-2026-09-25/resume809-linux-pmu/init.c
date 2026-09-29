#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_case(const char *policy, int round)
{
    printf("RESUME809_CASE_START policy=%s round=%d\n", policy, round);
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char *const argv[] = {
            "/fixed-count", "/bench", "--policy", (char *)policy,
            "--case", "thread_futex_same_cpu", NULL,
        };
        execv(argv[0], argv);
        perror("execv fixed-count");
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
    printf("RESUME809_CASE_DONE policy=%s round=%d exit=%d\n", policy, round, result);
    return result;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("RESUME809_INIT_START");
    int failures = 0;
    for (int round = 1; round <= 3; round++) {
        failures += run_case("other", round) != 0;
        failures += run_case("fifo", round) != 0;
    }
    printf("RESUME809_INIT_DONE failures=%d\n", failures);
    for (;;) {
        pause();
    }
}
