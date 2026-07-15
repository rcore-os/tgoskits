#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define EXPECTED_CPUS 4
#define MIGRATION_SPINS 10000
#define TIMER_WAKE_ROUNDS 8
#define TIMER_WAKE_NS 20000000L

static const char *const fail_marker = "STARRY_GICV2_SMP_FAILED";

static int current_cpu(void)
{
    unsigned int cpu = 0;
    unsigned int node = 0;

    if (syscall(SYS_getcpu, &cpu, &node, NULL) != 0) {
        return -1;
    }
    return (int)cpu;
}

static int sleep_on_cpu(int target_cpu)
{
    cpu_set_t affinity;
    CPU_ZERO(&affinity);
    CPU_SET(target_cpu, &affinity);
    if (sched_setaffinity(0, sizeof(affinity), &affinity) != 0) {
        fprintf(stderr, "%s: CPU%d sched_setaffinity: %s\n", fail_marker,
                target_cpu, strerror(errno));
        return 1;
    }

    int observed_cpu = -1;
    for (int spin = 0; spin < MIGRATION_SPINS; spin++) {
        observed_cpu = current_cpu();
        if (observed_cpu == target_cpu) {
            break;
        }
        if (observed_cpu < 0) {
            fprintf(stderr, "%s: CPU%d getcpu: %s\n", fail_marker,
                    target_cpu, strerror(errno));
            return 1;
        }
        sched_yield();
    }
    if (observed_cpu != target_cpu) {
        fprintf(stderr, "%s: task for CPU%d remained on CPU%d\n", fail_marker,
                target_cpu, observed_cpu);
        return 1;
    }

    for (int round = 0; round < TIMER_WAKE_ROUNDS; round++) {
        struct timespec remaining = {
            .tv_sec = 0,
            .tv_nsec = TIMER_WAKE_NS,
        };
        while (nanosleep(&remaining, &remaining) != 0) {
            if (errno != EINTR) {
                fprintf(stderr, "%s: CPU%d nanosleep: %s\n", fail_marker,
                        target_cpu, strerror(errno));
                return 1;
            }
        }

        observed_cpu = current_cpu();
        if (observed_cpu != target_cpu) {
            fprintf(stderr,
                    "%s: CPU%d timer wake resumed on CPU%d at round %d\n",
                    fail_marker, target_cpu, observed_cpu, round);
            return 1;
        }
    }
    return 0;
}

static void terminate_children(const pid_t *children, int count)
{
    for (int index = 0; index < count; index++) {
        if (children[index] > 0) {
            kill(children[index], SIGKILL);
        }
    }
    for (int index = 0; index < count; index++) {
        if (children[index] > 0) {
            (void)waitpid(children[index], NULL, 0);
        }
    }
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    long online_cpus = sysconf(_SC_NPROCESSORS_ONLN);
    if (online_cpus < EXPECTED_CPUS) {
        fprintf(stderr, "%s: expected %d online CPUs, found %ld\n",
                fail_marker, EXPECTED_CPUS, online_cpus);
        return 1;
    }

    pid_t children[EXPECTED_CPUS] = {0};
    for (int cpu = 0; cpu < EXPECTED_CPUS; cpu++) {
        pid_t child = fork();
        if (child < 0) {
            fprintf(stderr, "%s: fork CPU%d worker: %s\n", fail_marker, cpu,
                    strerror(errno));
            terminate_children(children, cpu);
            return 1;
        }
        if (child == 0) {
            _exit(sleep_on_cpu(cpu));
        }
        children[cpu] = child;
    }

    int failed = 0;
    for (int cpu = 0; cpu < EXPECTED_CPUS; cpu++) {
        int status = 0;
        if (waitpid(children[cpu], &status, 0) != children[cpu]) {
            fprintf(stderr, "%s: waitpid CPU%d worker: %s\n", fail_marker,
                    cpu, strerror(errno));
            failed = 1;
            continue;
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            fprintf(stderr, "%s: CPU%d worker status 0x%x\n", fail_marker,
                    cpu, status);
            failed = 1;
        }
    }

    if (failed) {
        return 1;
    }
    puts("STARRY_GICV2_SMP_PASSED");
    return 0;
}
