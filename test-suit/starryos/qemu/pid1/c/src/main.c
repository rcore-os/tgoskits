#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t handled;
static void handler(int signo) { handled = signo; }
static void require(int condition, const char *what)
{
    if (!condition) {
        printf("STARRY_PID1_FAILED: %s errno=%d\n", what, errno);
        fflush(stdout);
        if (getpid() != 1) _exit(1);
        syscall(SYS_reboot, 0xfee1dead, 672274793, RB_POWER_OFF, 0);
        for (;;) pause();
    }
}
static pthread_t leader;
static void *finish_after_leader_exit(void *unused)
{
    (void)unused;
    require(pthread_join(leader, NULL) == 0, "join init leader");
    /* The remaining init thread must still be able to fork and reap children. */
    pid_t child = fork();
    require(child >= 0, "fork after init leader exit");
    if (child == 0) _exit(19);
    int status;
    require(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 19,
            "reap after init leader exit");
    puts("STARRY_PID1_PASSED");
    sync();
    syscall(SYS_reboot, 0xfee1dead, 672274793, RB_POWER_OFF, 0);
    require(0, "poweroff returned");
    return NULL;
}

int main(void)
{
    setbuf(stdout, NULL);
    require(syscall(SYS_getpid) == 1 && syscall(SYS_getppid) == 0, "root init identity");
    puts("STARRY_PID1_BEGIN");
    /* Self-delivery is resolved before return from each syscall: no timing race. */
    require(syscall(SYS_kill, 1, SIGTERM) == 0, "default SIGTERM ignored");
    require(syscall(SYS_kill, 0, SIGTERM) == 0, "process-group default signal ignored");
    require(syscall(SYS_kill, 1, SIGKILL) == 0, "self SIGKILL ignored");
    require(syscall(SYS_tkill, 1, SIGKILL) == 0, "global init rejects SIGKILL");
    siginfo_t info = {.si_signo = SIGTERM, .si_code = SI_QUEUE, .si_pid = 1, .si_uid = 0};
    require(syscall(SYS_rt_sigqueueinfo, 1, SIGTERM, &info) == 0, "queued process signal ignored");
    require(syscall(SYS_rt_tgsigqueueinfo, 1, 1, SIGTERM, &info) == 0, "queued thread signal ignored");
    int pidfd = syscall(SYS_pidfd_open, 1, 0);
    require(pidfd >= 0, "open init pidfd");
    require(syscall(SYS_pidfd_send_signal, pidfd, SIGTERM, NULL, 0) == 0, "pidfd signal ignored");
    require(syscall(SYS_pidfd_send_signal, pidfd, SIGKILL, NULL, 0) == 0, "process SIGKILL ignored");
    close(pidfd);
    require(syscall(SYS_tgkill, 1, 1, SIGSTOP) == 0, "global init rejects SIGSTOP");
    struct sigaction action = {.sa_handler = handler};
    sigemptyset(&action.sa_mask);
    require(sigaction(SIGUSR1, &action, NULL) == 0, "install handler");
    require(syscall(SYS_kill, 1, SIGUSR1) == 0 && handled == SIGUSR1, "handled signal delivered");
    sigset_t blocked, pending;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR2);
    require(sigprocmask(SIG_BLOCK, &blocked, NULL) == 0, "block default signal");
    require(syscall(SYS_kill, 1, SIGUSR2) == 0, "queue sigwait signal");
    struct timespec zero = {0};
    require(syscall(SYS_rt_sigtimedwait, &blocked, &info, &zero, sizeof(unsigned long)) == SIGUSR2, "sigwait consumes blocked init signal");
    require(syscall(SYS_tgkill, 1, 1, SIGUSR2) == 0, "queue blocked signal");
    require(sigpending(&pending) == 0 && sigismember(&pending, SIGUSR2), "blocked signal remains pending");
    require(sigaction(SIGUSR2, &action, NULL) == 0, "install pending signal handler");
    require(sigprocmask(SIG_UNBLOCK, &blocked, NULL) == 0 && handled == SIGUSR2, "deliver after disposition change");
    action.sa_flags = SA_RESTART;
    require(sigaction(SIGCHLD, &action, NULL) == 0, "install SIGCHLD handler");
    int fds[2];
    require(pipe(fds) == 0, "pipe");
    pid_t child = fork();
    require(child >= 0, "fork");
    if (child == 0) {
        close(fds[0]);
        pid_t orphan = fork();
        if (orphan < 0) _exit(2);
        if (orphan > 0) _exit(0);
        while (getppid() != 1) syscall(SYS_sched_yield);
        pid_t parent = getppid();
        if (write(fds[1], &parent, sizeof(parent)) != sizeof(parent)) _exit(3);
        _exit(23);
    }
    close(fds[1]);
    pid_t adopted_by = 0;
    require(read(fds[0], &adopted_by, sizeof(adopted_by)) == sizeof(adopted_by) && adopted_by == 1, "orphan adoption");
    close(fds[0]);
    int seen_parent = 0, seen_orphan = 0;
    for (int i = 0; i < 2; ++i) {
        int status;
        pid_t reaped = waitpid(-1, &status, 0);
        require(reaped > 0 && WIFEXITED(status), "reap children");
        if (reaped == child) seen_parent = WEXITSTATUS(status) == 0;
        else seen_orphan = WEXITSTATUS(status) == 23;
    }
    require(seen_parent && seen_orphan, "orphan exit status");
    require(handled == SIGCHLD, "SIGCHLD delivery");
    errno = 0;
    require(waitpid(-1, NULL, WNOHANG) == -1 && errno == ECHILD, "no zombie remains");
    leader = pthread_self();
    pthread_t worker;
    require(pthread_create(&worker, NULL, finish_after_leader_exit, NULL) == 0, "create init peer");
    pthread_exit(NULL);
}
