/*
 * clone3-badsize: trigger the StarryOS clone3 size handling bug.
 *
 * Linux-compatible behavior: clone3 should ignore unknown trailing bytes in
 * the user-supplied struct and either succeed or return a normal errno.
 * StarryOS bug: a size larger than struct clone_args overflows the kernel
 * buffer slice and can panic the kernel.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_clone3
#define SYS_clone3 435
#endif

struct clone_args {
    uint64_t flags;
    uint64_t pidfd;
    uint64_t child_tid;
    uint64_t parent_tid;
    uint64_t exit_signal;
    uint64_t stack;
    uint64_t stack_size;
    uint64_t tls;
    uint64_t set_tid;
    uint64_t set_tid_size;
    uint64_t cgroup;
};

static int do_clone3_overlong(void)
{
    struct clone_args args;
    memset(&args, 0, sizeof(args));
    args.exit_signal = SIGCHLD;

    size_t size = sizeof(args) + 8;
    long ret = syscall(SYS_clone3, &args, size);
    if (ret < 0) {
        printf("clone3 returned errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (ret == 0) {
        _exit(0);
    }

    int status = 0;
    if (waitpid((pid_t)ret, &status, 0) < 0) {
        printf("waitpid failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        printf("child exited abnormally: status=0x%x\n", status);
        return 1;
    }

    return 0;
}

int main(void)
{
    printf("=== clone3-badsize ===\n");
    printf("Calling clone3 with size larger than struct clone_args...\n");

    if (do_clone3_overlong() != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    printf("TEST PASSED\n");
    return 0;
}