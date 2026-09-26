#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_case(const char *event, const char *policy, int round)
{
    printf("RESUME813_CASE_START event=%s policy=%s round=%d\n",
           event, policy, round);
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 1;
    }
    if (child == 0) {
        char *const argv[] = {
            "/bench-stratified-pmu", "--policy", (char *)policy,
            "--case", "thread_futex_same_cpu", NULL,
        };
        if (setenv("WAKEUP_PMU_EVENT", event, 1) != 0) {
            perror("setenv");
            _exit(127);
        }
        execv(argv[0], argv);
        perror("execv bench-stratified-pmu");
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
    printf("RESUME813_CASE_DONE event=%s policy=%s round=%d exit=%d\n",
           event, policy, round, result);
    return result;
}

int main(void)
{
    static const char *const events[] = {
        "instructions", "cycles", "l1i_refill",
    };
    static const char *const policies[] = {"other", "fifo"};
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("RESUME813_LINUX_START");
    int failures = 0;
    for (size_t event = 0; event < sizeof(events) / sizeof(events[0]); event++) {
        for (int round = 1; round <= 2; round++) {
            for (size_t policy = 0; policy < 2; policy++) {
                failures += run_case(events[event], policies[policy], round) != 0;
            }
        }
    }
    printf("RESUME813_LINUX_DONE failures=%d\n", failures);
    for (;;) {
        pause();
    }
}
