#define _GNU_SOURCE

/*
 * Linux exposes the process referenced by a pidfd through the Pid and NSpid
 * fields in /proc/self/fdinfo/<fd>. systemd uses the Pid field immediately
 * after pidfd_spawn() to construct a race-free process reference.
 */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLONE_PIDFD
#define CLONE_PIDFD 0x00001000ULL
#endif

struct clone_args {
    unsigned long long flags;
    unsigned long long pidfd;
    unsigned long long child_tid;
    unsigned long long parent_tid;
    unsigned long long exit_signal;
    unsigned long long stack;
    unsigned long long stack_size;
    unsigned long long tls;
    unsigned long long set_tid;
    unsigned long long set_tid_size;
    unsigned long long cgroup;
};

static int read_pid_field(FILE *stream, const char *field, pid_t *value)
{
    char line[128];
    size_t field_length = strlen(field);

    rewind(stream);
    while (fgets(line, sizeof(line), stream) != NULL) {
        if (strncmp(line, field, field_length) != 0 || line[field_length] != ':')
            continue;

        char *end = NULL;
        errno = 0;
        long parsed = strtol(line + field_length + 1, &end, 10);
        if (errno != 0 || end == line + field_length + 1 || parsed <= 0)
            return -1;

        *value = (pid_t)parsed;
        return 0;
    }

    return -1;
}

int main(void)
{
    int pidfd = -1;
    struct clone_args args = {
        .flags = CLONE_PIDFD,
        .pidfd = (unsigned long long)&pidfd,
        .exit_signal = SIGCHLD,
    };

    pid_t child = (pid_t)syscall(SYS_clone3, &args, sizeof(args));
    if (child == 0) {
        for (;;)
            pause();
    }
    if (child < 0 || pidfd < 0) {
        fprintf(stderr, "FAIL: clone3(CLONE_PIDFD): errno=%d (%s) pidfd=%d\n",
                errno, strerror(errno), pidfd);
        return 1;
    }

    char path[64];
    snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", pidfd);
    FILE *stream = fopen(path, "re");
    if (stream == NULL) {
        fprintf(stderr, "FAIL: open %s: errno=%d (%s)\n", path, errno,
                strerror(errno));
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        close(pidfd);
        return 1;
    }

    pid_t reported_pid = -1;
    pid_t reported_nspid = -1;
    int pid_result = read_pid_field(stream, "Pid", &reported_pid);
    int nspid_result = read_pid_field(stream, "NSpid", &reported_nspid);
    fclose(stream);

    kill(child, SIGKILL);
    int status = 0;
    int waited = waitpid(child, &status, 0);
    close(pidfd);

    if (pid_result != 0 || reported_pid != child) {
        fprintf(stderr, "FAIL: pidfd fdinfo Pid=%d expected=%d\n",
                reported_pid, child);
        return 1;
    }
    if (nspid_result != 0 || reported_nspid != child) {
        fprintf(stderr, "FAIL: pidfd fdinfo NSpid=%d expected=%d\n",
                reported_nspid, child);
        return 1;
    }
    if (waited != child || !WIFSIGNALED(status) || WTERMSIG(status) != SIGKILL) {
        fprintf(stderr, "FAIL: child cleanup status=%d waited=%d expected=%d\n",
                status, waited, child);
        return 1;
    }

    printf("PASS: pidfd fdinfo Pid=%d NSpid=%d\n", reported_pid,
           reported_nspid);
    puts("STARRY_PIDFD_FDINFO_PASSED");
    return 0;
}
