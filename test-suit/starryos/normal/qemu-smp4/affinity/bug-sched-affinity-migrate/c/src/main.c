#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define READY_TOKEN 0x5a
#define DONE_TOKEN  0xa5
#define MAX_SPINS   200000

static void die(const char *msg)
{
    printf("TEST FAILED: %s: %s\n", msg, strerror(errno));
    fflush(stdout);
    abort();
}

static cpu_set_t single_cpu(int cpu)
{
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    return set;
}

static int read_current_processor(void)
{
    FILE *fp = fopen("/proc/self/stat", "r");
    if (fp == NULL) {
        die("open /proc/self/stat");
    }

    int pid;
    char comm[64];
    char state;
    unsigned long long fields[36];
    int matched = fscanf(fp,
                         "%d %63s %c %llu %llu %llu %llu %llu %llu %llu "
                         "%llu %llu %llu %llu %llu %llu %llu %llu %llu %llu "
                         "%llu %llu %llu %llu %llu %llu %llu %llu %llu %llu "
                         "%llu %llu %llu %llu %llu %llu %llu %llu %llu",
                         &pid, comm, &state, &fields[0], &fields[1], &fields[2],
                         &fields[3], &fields[4], &fields[5], &fields[6],
                         &fields[7], &fields[8], &fields[9], &fields[10],
                         &fields[11], &fields[12], &fields[13], &fields[14],
                         &fields[15], &fields[16], &fields[17], &fields[18],
                         &fields[19], &fields[20], &fields[21], &fields[22],
                         &fields[23], &fields[24], &fields[25], &fields[26],
                         &fields[27], &fields[28], &fields[29], &fields[30],
                         &fields[31], &fields[32], &fields[33], &fields[34],
                         &fields[35]);
    fclose(fp);
    if (matched != 39) {
        errno = EINVAL;
        die("parse /proc/self/stat");
    }

    return (int)fields[35];
}

static void write_token(int fd, unsigned char token)
{
    if (write(fd, &token, 1) != 1) {
        die("write token");
    }
}

static void read_token(int fd, unsigned char expected)
{
    unsigned char token = 0;
    if (read(fd, &token, 1) != 1) {
        die("read token");
    }
    if (token != expected) {
        errno = EPROTO;
        die("unexpected token");
    }
}

int main(void)
{
    setbuf(stdout, NULL);
    printf("TEST START: sched affinity migrates running task\n");

    long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
    if (ncpu < 2) {
        printf("TEST SKIPPED: expected at least two CPUs\n");
        return 0;
    }

    int ready_pipe[2];
    int done_pipe[2];
    if (pipe(ready_pipe) != 0 || pipe(done_pipe) != 0) {
        die("pipe");
    }

    pid_t child = fork();
    if (child < 0) {
        die("fork");
    }

    if (child == 0) {
        close(ready_pipe[0]);
        close(done_pipe[0]);

        cpu_set_t cpu0 = single_cpu(0);
        if (sched_setaffinity(0, sizeof(cpu0), &cpu0) != 0) {
            die("child set affinity CPU0");
        }

        for (int i = 0; i < MAX_SPINS; i++) {
            if (read_current_processor() == 0) {
                write_token(ready_pipe[1], READY_TOKEN);
                for (int j = 0; j < MAX_SPINS; j++) {
                    sched_yield();
                    if (read_current_processor() == 1) {
                        write_token(done_pipe[1], DONE_TOKEN);
                        _exit(0);
                    }
                }
                printf("TEST FAILED: child never migrated to CPU1 after sched_setaffinity\n");
                _exit(1);
            }
            sched_yield();
        }

        printf("TEST FAILED: child never observed itself on CPU0\n");
        _exit(1);
    }

    close(ready_pipe[1]);
    close(done_pipe[1]);

    read_token(ready_pipe[0], READY_TOKEN);

    cpu_set_t cpu1 = single_cpu(1);
    if (sched_setaffinity(child, sizeof(cpu1), &cpu1) != 0) {
        kill(child, SIGKILL);
        die("parent set child affinity CPU1");
    }

    read_token(done_pipe[0], DONE_TOKEN);

    int status;
    if (waitpid(child, &status, 0) < 0) {
        die("waitpid");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        errno = ECHILD;
        die("child status");
    }

    printf("TEST PASSED\n");
    return 0;
}
