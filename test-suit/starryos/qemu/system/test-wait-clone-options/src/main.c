#define _GNU_SOURCE

#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __WCLONE
#define __WCLONE 0x80000000
#endif
#ifndef __WALL
#define __WALL 0x40000000
#endif

#define STACK_SIZE (64 * 1024)

static int passed;
static int failed;

static void note_pass(const char *name)
{
    printf("PASS: %s\n", name);
    passed++;
}

static void note_fail(const char *name, const char *detail)
{
    printf("FAIL: %s: %s\n", name, detail);
    failed++;
}

static void sigusr1_handler(int signo)
{
    (void)signo;
}

static int exit_code_child(void *arg)
{
    _exit((int)(long)arg);
}

static pid_t spawn_clone_child(int exit_code, int exit_signal, void **stack_out)
{
    void *stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (stack == MAP_FAILED) {
        return -1;
    }

    pid_t pid = clone(exit_code_child, (char *)stack + STACK_SIZE, exit_signal,
                      (void *)(long)exit_code);
    if (pid < 0) {
        munmap(stack, STACK_SIZE);
        return -1;
    }
    *stack_out = stack;
    return pid;
}

static int wait_for_exit(pid_t pid, int options, int expected_status,
                         const char *name)
{
    int status = 0;
    errno = 0;
    pid_t got = waitpid(pid, &status, options);
    if (got != pid) {
        char buf[160];
        snprintf(buf, sizeof(buf), "waitpid got=%ld errno=%d (%s), expected %ld",
                 (long)got, errno, strerror(errno), (long)pid);
        note_fail(name, buf);
        return -1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != expected_status) {
        char buf[160];
        snprintf(buf, sizeof(buf), "status=0x%x, expected exit %d", status,
                 expected_status);
        note_fail(name, buf);
        return -1;
    }
    note_pass(name);
    return 0;
}

static void cleanup_stack(void **stack)
{
    if (*stack != NULL && *stack != MAP_FAILED) {
        munmap(*stack, STACK_SIZE);
        *stack = NULL;
    }
}

static void test_clone_filter_waitpid(void)
{
    void *sigchld_stack = NULL;
    void *clone_stack = NULL;
    pid_t sigchld_child = spawn_clone_child(21, SIGCHLD, &sigchld_stack);
    pid_t clone_child = spawn_clone_child(22, SIGUSR1, &clone_stack);
    if (sigchld_child < 0 || clone_child < 0) {
        note_fail("setup waitpid clone filters", "clone failed");
        if (sigchld_child > 0) waitpid(sigchld_child, NULL, __WALL);
        if (clone_child > 0) waitpid(clone_child, NULL, __WALL);
        cleanup_stack(&sigchld_stack);
        cleanup_stack(&clone_stack);
        return;
    }

    if (wait_for_exit(sigchld_child, 0, 21,
                      "waitpid default reaps SIGCHLD child") != 0) {
        waitpid(clone_child, NULL, __WALL);
        cleanup_stack(&sigchld_stack);
        cleanup_stack(&clone_stack);
        return;
    }

    errno = 0;
    int status = 0;
    pid_t got = waitpid(clone_child, &status, WNOHANG);
    if (got == -1 && errno == ECHILD) {
        note_pass("waitpid default ignores non-SIGCHLD clone child");
    } else {
        char buf[160];
        snprintf(buf, sizeof(buf), "waitpid got=%ld status=0x%x errno=%d (%s)",
                 (long)got, status, errno, strerror(errno));
        note_fail("waitpid default ignores non-SIGCHLD clone child", buf);
    }

    wait_for_exit(clone_child, __WCLONE, 22,
                  "waitpid __WCLONE reaps non-SIGCHLD clone child");
    cleanup_stack(&sigchld_stack);
    cleanup_stack(&clone_stack);
}

static void test_wall_waitpid(void)
{
    void *stack = NULL;
    pid_t pid = spawn_clone_child(23, SIGUSR1, &stack);
    if (pid < 0) {
        note_fail("waitpid __WALL setup", "clone failed");
        cleanup_stack(&stack);
        return;
    }
    wait_for_exit(pid, __WALL, 23, "waitpid __WALL reaps clone child");
    cleanup_stack(&stack);
}

static void test_waitid_clone_filter(void)
{
    void *stack = NULL;
    pid_t pid = spawn_clone_child(24, SIGUSR1, &stack);
    if (pid < 0) {
        note_fail("waitid __WCLONE setup", "clone failed");
        cleanup_stack(&stack);
        return;
    }

    siginfo_t si;
    memset(&si, 0, sizeof(si));
    errno = 0;
    int ret = waitid(P_PID, pid, &si, WEXITED | WNOHANG);
    if (ret == -1 && errno == ECHILD) {
        note_pass("waitid default ignores non-SIGCHLD clone child");
    } else {
        char buf[200];
        snprintf(buf, sizeof(buf), "ret=%d si_pid=%ld errno=%d (%s)", ret,
                 (long)si.si_pid, errno, strerror(errno));
        note_fail("waitid default ignores non-SIGCHLD clone child", buf);
    }

    memset(&si, 0, sizeof(si));
    errno = 0;
    ret = waitid(P_PID, pid, &si, WEXITED | __WCLONE);
    if (ret == 0 && si.si_pid == pid && si.si_code == CLD_EXITED &&
        si.si_status == 24) {
        note_pass("waitid __WCLONE reaps non-SIGCHLD clone child");
    } else {
        char buf[240];
        snprintf(buf, sizeof(buf),
                 "ret=%d si_pid=%ld si_code=%d si_status=%d errno=%d (%s)",
                 ret, (long)si.si_pid, si.si_code, si.si_status, errno,
                 strerror(errno));
        note_fail("waitid __WCLONE reaps non-SIGCHLD clone child", buf);
        waitpid(pid, NULL, __WALL);
    }
    cleanup_stack(&stack);
}

int main(void)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = sigusr1_handler;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    sigset_t blocked;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR1);
    sigprocmask(SIG_BLOCK, &blocked, NULL);

    printf("=== test-wait-clone-options ===\n");
    test_clone_filter_waitpid();
    test_wall_waitpid();
    test_waitid_clone_filter();
    printf("=== test-wait-clone-options: passed=%d failed=%d ===\n",
           passed, failed);
    return failed == 0 ? 0 : 1;
}
