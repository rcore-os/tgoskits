#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

struct case_run {
    const char *binary;
    const char *policy;
    int round;
};

static int run_case(const struct case_run *run)
{
    printf("RESUME841_CASE_START binary=%s policy=%s round=%d\n",
           run->binary, run->policy, run->round);
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        const char *path = run->binary;
        char *const argv[] = {(char *)path, "--policy", (char *)run->policy,
                              "--case", "thread_futex_same_cpu", NULL};
        execv(path, argv);
        perror("execv benchmark");
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
    printf("RESUME841_CASE_DONE binary=%s policy=%s round=%d exit=%d\n",
           run->binary, run->policy, run->round, result);
    return result;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("RESUME841_INIT_START");
    static const struct case_run cases[] = {
        {"/control", "fifo", 1},
        {"/forced", "fifo", 1},
        {"/frozen", "fifo", 1},
        {"/control", "other", 1},
        {"/control", "other", 2},
        {"/frozen", "fifo", 2},
        {"/forced", "fifo", 2},
        {"/control", "fifo", 2},
    };
    int failures = 0;
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        failures += run_case(&cases[index]) != 0;
    }
    printf("RESUME841_INIT_DONE failures=%d\n", failures);
    for (;;) {
        pause();
    }
}
