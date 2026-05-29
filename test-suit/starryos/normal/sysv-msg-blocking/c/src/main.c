#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/wait.h>
#include <unistd.h>

struct test_msg {
    long mtype;
    char mtext[8192];
};

static int failures;

static void check(int condition, const char *msg)
{
    if (condition) {
        printf("PASS: %s\n", msg);
    } else {
        printf("FAIL: %s errno=%d (%s)\n", msg, errno, strerror(errno));
        failures++;
    }
}

static void checked_msgctl_rmid(int msqid)
{
    if (msgctl(msqid, IPC_RMID, NULL) == -1 && errno != EINVAL && errno != EIDRM) {
        printf("WARN: msgctl IPC_RMID failed errno=%d (%s)\n", errno, strerror(errno));
    }
}

static int wait_for_child(pid_t pid, const char *msg)
{
    int status = 0;
    pid_t waited;

    do {
        waited = waitpid(pid, &status, 0);
    } while (waited == -1 && errno == EINTR);

    check(waited == pid, msg);
    if (waited != pid) {
        return 1;
    }

    check(WIFEXITED(status) && WEXITSTATUS(status) == 0, msg);
    return !(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static void test_blocking_receive(void)
{
    int msqid = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    check(msqid >= 0, "msgget for blocking receive");
    if (msqid < 0) {
        return;
    }

    pid_t pid = fork();
    check(pid >= 0, "fork receiver");
    if (pid < 0) {
        checked_msgctl_rmid(msqid);
        return;
    }
    if (pid == 0) {
        struct test_msg msg;
        ssize_t n = msgrcv(msqid, &msg, sizeof(msg.mtext), 7, 0);
        _exit(n == 5 && msg.mtype == 7 && memcmp(msg.mtext, "hello", 5) == 0 ? 0 : 1);
    }

    usleep(50000);
    struct test_msg msg = {.mtype = 7};
    memcpy(msg.mtext, "hello", 5);
    check(msgsnd(msqid, &msg, 5, 0) == 0, "msgsnd wakes blocking receiver");
    failures += wait_for_child(pid, "blocking receiver exits successfully");
    checked_msgctl_rmid(msqid);
}

static void test_blocking_send(void)
{
    int msqid = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    check(msqid >= 0, "msgget for blocking send");
    if (msqid < 0) {
        return;
    }

    struct test_msg msg = {.mtype = 1};
    memset(msg.mtext, 'A', sizeof(msg.mtext));

    check(msgsnd(msqid, &msg, sizeof(msg.mtext), 0) == 0, "fill queue first chunk");
    check(msgsnd(msqid, &msg, sizeof(msg.mtext), 0) == 0, "fill queue second chunk");

    pid_t pid = fork();
    check(pid >= 0, "fork sender");
    if (pid < 0) {
        checked_msgctl_rmid(msqid);
        return;
    }
    if (pid == 0) {
        struct test_msg child_msg = {.mtype = 2};
        memcpy(child_msg.mtext, "wakeup", 6);
        _exit(msgsnd(msqid, &child_msg, 6, 0) == 0 ? 0 : 1);
    }

    usleep(50000);
    struct test_msg recv_msg;
    ssize_t n = msgrcv(msqid, &recv_msg, sizeof(recv_msg.mtext), 0, 0);
    check(n == (ssize_t)sizeof(msg.mtext), "msgrcv frees queue space");
    failures += wait_for_child(pid, "blocking sender exits successfully");

    n = msgrcv(msqid, &recv_msg, sizeof(recv_msg.mtext), 2, IPC_NOWAIT);
    check(n == 6 && memcmp(recv_msg.mtext, "wakeup", 6) == 0,
          "queued message from unblocked sender is readable");
    checked_msgctl_rmid(msqid);
}

static void test_rmid_wakes_receiver(void)
{
    int msqid = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    check(msqid >= 0, "msgget for IPC_RMID wake");
    if (msqid < 0) {
        return;
    }

    pid_t pid = fork();
    check(pid >= 0, "fork receiver for IPC_RMID");
    if (pid < 0) {
        checked_msgctl_rmid(msqid);
        return;
    }
    if (pid == 0) {
        struct test_msg msg;
        ssize_t n = msgrcv(msqid, &msg, sizeof(msg.mtext), 1, 0);
        _exit(n == -1 && errno == EIDRM ? 0 : 1);
    }

    usleep(50000);
    check(msgctl(msqid, IPC_RMID, NULL) == 0, "IPC_RMID removes queue");
    failures += wait_for_child(pid, "IPC_RMID wakes blocking receiver");
}

int main(void)
{
    alarm(10);

    test_blocking_receive();
    test_blocking_send();
    test_rmid_wakes_receiver();

    if (failures == 0) {
        printf("sysv-msg-blocking: ok\n");
        return 0;
    }

    printf("sysv-msg-blocking: failed (%d)\n", failures);
    return 1;
}
