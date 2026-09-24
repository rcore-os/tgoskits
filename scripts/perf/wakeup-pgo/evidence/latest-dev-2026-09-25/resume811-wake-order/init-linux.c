#include <errno.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_case(const char *policy, int round)
{
    printf("RESUME811_CASE_START policy=%s round=%d\n", policy, round);
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char *const argv[] = {
            "/bench-order", "--policy", (char *)policy,
            "--case", "thread_futex_same_cpu", NULL,
        };
        execv(argv[0], argv);
        perror("execv bench-order");
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
    printf("RESUME811_CASE_DONE policy=%s round=%d exit=%d\n",
           policy, round, result);
    return result;
}

int main(void)
{
    static const char *const policies[] = {"other", "fifo"};
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("RESUME811_LINUX_START");
    int failures = 0;
    for (int round = 1; round <= 3; round++) {
        for (size_t policy = 0; policy < 2; policy++) {
            failures += run_case(policies[policy], round) != 0;
        }
    }
    printf("RESUME811_LINUX_DONE failures=%d\n", failures);
    for (;;) {
        pause();
    }
}
